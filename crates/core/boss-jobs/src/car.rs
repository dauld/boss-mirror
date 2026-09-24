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

/// The ship-a-change packet body for a car. `owner` is who answers
/// for it — the platform owner as `boss_core::platform_owner` resolved
/// it, or `NOBODY` when it refused (backlog 3c23662d: this line named
/// one person, on every car on every deployment, until 2026-09-18).
pub fn car_body(
    branch: &str,
    summary: &str,
    backlog_item: Option<&str>,
    delivery_channel: Option<&str>,
    owner: &str,
) -> Value {
    let mut metadata = json!({ "branch": branch, "summary": summary });
    if let Some(item) = backlog_item {
        // A declared job edge — ref-checked by the API at the write,
        // which is what makes it safe to write here rather than by hand.
        // A mistyped id is refused instead of silently pointing at
        // nothing.
        metadata[BACKLOG_ITEM] = json!(item);
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
        "owner_id": owner,
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

/// One step completion a car's filer owes: which step, the evidence to
/// MERGE onto it, and the status-only body that then completes it.
///
/// TWO WRITES, IN THIS ORDER (backlog e39a9d2a, car 2 of its plan). This
/// was one PUT of `{status, metadata}` built fresh, with no read. The
/// step PUT REPLACES metadata wholesale, and the registry materializes
/// keys onto every step at admission (`metadata_defaults`,
/// `authority_role`, `station`, `audience`, `claimable`), so every
/// `boss car open`, `boss park` and auto-park shed them. Now the
/// evidence goes through the step merge door — [`StepWrite::merge_path`],
/// one transaction against the row as it stands, so it cannot race — and
/// then [`StepWrite::status_path`] takes a body carrying nothing to
/// drop. Merge FIRST: a step's required-at-done fields are validated
/// when it flips to completed, so the evidence must already be there.
///
/// The step ID is read OFF the car, never assembled — the same rule the
/// receipt lives by, for the same reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepWrite {
    /// The step's id, as the packet reported it.
    pub step_id: String,
    /// Its title, so a message can name what was written.
    pub title: &'static str,
    /// `PATCH /api/jobs/{car}/steps/{step_id}/metadata` body: top-level
    /// keys merged into what the step already holds.
    pub metadata: Value,
    /// `PUT /api/jobs/{car}/steps/{step_id}` body, sent AFTER the merge:
    /// the status alone.
    pub status_body: Value,
}

impl StepWrite {
    fn completing(step_id: &str, title: &'static str, metadata: Value) -> Self {
        Self {
            step_id: step_id.to_string(),
            title,
            metadata,
            status_body: json!({"status": "completed"}),
        }
    }

    /// The step merge door on `job_id` — the FIRST write. The job is the
    /// car for its own steps, and the linked item for
    /// [`triage_on_park`]'s.
    pub fn merge_path(&self, job_id: &str) -> String {
        format!("/api/jobs/{job_id}/steps/{}/metadata", self.step_id)
    }

    /// The step PUT on `job_id` — the SECOND write, carrying
    /// [`StepWrite::status_body`].
    pub fn status_path(&self, job_id: &str) -> String {
        format!("/api/jobs/{job_id}/steps/{}", self.step_id)
    }
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
    Ok(vec![StepWrite::completing(
        step_id,
        SCOPE,
        json!({"summary": summary, "excludes": excludes, "completed_at": at}),
    )])
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
        out.push(StepWrite::completing(step_id, title, metadata));
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

/// THE NOT-YET STREAK (backlog adef5ddf). A car's `proof_attempt` is
/// REPLACED on every run, so until 2026-09-23 it said what the last run
/// answered and nothing about how long it had been answering it. `exit
/// 75` means "early, not wrong", and a probe that can NEVER pass says
/// exactly that too: car b8c4267f greps a literal a later car removed on
/// 9a grounds, car 52e0287e takes the last of four matches where the
/// call is the second. Both were counted as patiently waiting and
/// rechecked hourly — measured over the 1812 ops-requests on record
/// (2026-09-17 to 09-23): six open cars at 86 to 88 consecutive
/// not-yets each, spanning 97h to 134h, and no probe run ever answered
/// them otherwise. The exit code cannot tell starving from waiting; the
/// length of the streak is the only signal there is, so each attempt now
/// carries where its streak began and how many runs it holds.
///
/// Written by the doors that record an attempt
/// (`boss prove`'s `attempt_json`), read by [`not_yet_streak`].
pub const NOT_YET_SINCE: &str = "not_yet_since";
/// How many consecutive runs of the same probe have answered not-yet,
/// this one included. See [`NOT_YET_SINCE`].
pub const NOT_YET_RUNS: &str = "not_yet_runs";

/// Did this recorded attempt answer NOT YET? The flag the doors stamp,
/// or a bare exit 75 on records older than the flag — one definition,
/// read by the shed's classification and the streak both.
pub fn attempt_said_not_yet(attempt: &Value) -> bool {
    attempt.get("not_yet").and_then(Value::as_bool) == Some(true)
        || attempt.get("exit").and_then(Value::as_i64) == Some(75)
}

/// Where a not-yet run's streak began and how many runs it now holds,
/// given the car's PRIOR attempt: `(since, runs)`, for a run at `at` of
/// `probe`. The streak continues only across not-yets of the SAME probe
/// text — a corrected probe is the repair for a starved one, and must
/// not inherit its four days. A prior record written before these keys
/// existed dates the streak from its own `at`, the earliest not-yet this
/// door can vouch for.
pub fn carried_not_yet_streak(prior: Option<&Value>, probe: &str, at: &str) -> (String, u64) {
    let continuing = prior.filter(|p| {
        attempt_said_not_yet(p) && p.get("probe").and_then(Value::as_str) == Some(probe)
    });
    match continuing {
        Some(p) => {
            let since = p
                .get(NOT_YET_SINCE)
                .and_then(Value::as_str)
                .or_else(|| p.get("at").and_then(Value::as_str))
                .unwrap_or(at);
            let runs = p
                .get(NOT_YET_RUNS)
                .and_then(Value::as_u64)
                .filter(|n| *n > 0)
                .unwrap_or(1);
            (since.to_string(), runs + 1)
        }
        None => (at.to_string(), 1),
    }
}

/// A car's not-yet streak, read back: how many hours lie between its
/// first and its latest not-yet, and how many runs answered it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NotYetStreak {
    pub hours: i64,
    pub runs: u64,
}

/// The streak on a car's metadata, if its last run answered not-yet.
///
/// Measured between two RUNS, never against the clock: a recheck that
/// stopped firing must not age a streak nobody is asking. And `None`
/// when the car's recorded probe is no longer the text that ran, so a
/// probe corrected by a metadata PATCH stops reading as starved at once
/// rather than at the next hourly write.
pub fn not_yet_streak(md: &Value) -> Option<NotYetStreak> {
    let attempt = md.get("proof_attempt")?;
    if !attempt_said_not_yet(attempt) {
        return None;
    }
    let ran = attempt.get("probe").and_then(Value::as_str);
    let recorded = md.get(PROOF_PROBE).and_then(Value::as_str);
    if let (Some(ran), Some(recorded)) = (ran, recorded)
        && ran != recorded
    {
        return None;
    }
    let instant = |k: &str| {
        attempt
            .get(k)
            .and_then(Value::as_str)
            .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
    };
    let last = instant("at")?;
    let since = instant(NOT_YET_SINCE).unwrap_or(last);
    let runs = attempt
        .get(NOT_YET_RUNS)
        .and_then(Value::as_u64)
        .filter(|n| *n > 0)
        .unwrap_or(1);
    Some(NotYetStreak {
        hours: (last - since).num_hours(),
        runs,
    })
}

/// WHAT THE CAR SAYS IT WAITS ON (backlog b461341d). Duration alone
/// cannot tell stuck from patient: the operator triage of the six cars
/// adef5ddf's streak measured past its bound (b8c4267f, 4b05fe3e,
/// f71a3c90, b94cb42f, 59398050, 7f01b854, 2026-09-23) found ALL SIX
/// honestly waiting on the world — a publish failure that has not
/// happened, a tenant publish, a red crawl, a release David has not
/// opened, a destructive prune that is his call, a real Stripe charge —
/// and none with a wrong probe. So about three days after that bound
/// converged the shed would have called six honest waits "ours to read",
/// the false signal the label exists to prevent, pointed the other way.
///
/// So the car SAYS what it waits on, as `{"on": <prose naming the event
/// or the actor>, "seen": <optional shell text>}`. `seen` is a second,
/// smaller probe, run by the same door and on the same host as the
/// proof probe whenever that one answers not-yet, which exits 0 once the
/// awaited event is in the record. It is what turns the declaration from
/// a belief into something the record can contradict: a probe still
/// saying not-yet AFTER its own declared event was seen is the true
/// "ours to read", at any streak length. Written by `boss car waits-on`
/// (or the metadata PATCH it wraps), read by [`starved`].
pub const WAITS_ON: &str = "waits_on";
/// On a not-yet `proof_attempt`: when the car's declared `seen` check
/// first exited 0 in an unbroken run of sightings, or `null`. Carried by
/// [`carried_seen_at`].
pub const WAITS_ON_SEEN_AT: &str = "waits_on_seen_at";

/// A car's declared wait, read back. `None` for no declaration, and for
/// one that names nothing — a blank `on` must not silence the label by
/// merely being present.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WaitsOn {
    pub on: String,
    pub seen: Option<String>,
}

fn non_blank(v: Option<&Value>) -> Option<String> {
    v.and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

pub fn waits_on(md: &Value) -> Option<WaitsOn> {
    let w = md.get(WAITS_ON)?;
    Some(WaitsOn {
        on: non_blank(w.get("on"))?,
        seen: non_blank(w.get("seen")),
    })
}

/// The declaration in the one shape [`waits_on`] reads — what the verb
/// PATCHes, and what an operator writing the PATCH by hand should copy.
pub fn waits_on_value(on: &str, seen: Option<&str>) -> Value {
    let seen = seen.map(str::trim).filter(|s| !s.is_empty());
    json!({"on": on.trim(), "seen": seen})
}

/// A written declaration MERGED into the one the car carries (backlog
/// e9b164a1 piece 3): each field `update` carries replaces that field,
/// every other field `existing` declares is kept. `None` when the result
/// names no `on` — a declaration of nothing, which neither writer may
/// record. One definition for both writers, `boss car waits-on` and the
/// auto-park handler's copy of `--park-waits-on`, because the object is
/// wider than its first two fields now: an `owner` and a patience
/// written by hand must survive a writer that re-states only `on`, and
/// the metadata door merges top-level keys only.
pub fn merge_waits_on(existing: Option<&Value>, update: &Value) -> Option<Value> {
    let mut merged = existing
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    merged.extend(update.as_object()?.clone());
    let merged = Value::Object(merged);
    waits_on(&json!({ WAITS_ON: merged.clone() }))?;
    Some(merged)
}

/// When the declared event was first seen, for a run that `seen` it (or
/// not): the prior attempt's sighting if it had one, else this run's
/// instant; `None` for a run that did not see it — a sighting that stops
/// is not a sighting.
pub fn carried_seen_at(prior: Option<&Value>, seen: bool, at: &str) -> Option<String> {
    if !seen {
        return None;
    }
    Some(
        prior
            .and_then(|p| p.get(WAITS_ON_SEEN_AT))
            .and_then(Value::as_str)
            .unwrap_or(at)
            .to_string(),
    )
}

/// Why a not-yet car is ours to read rather than the world's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Starved {
    /// No declared wait, and the probe has said not-yet without a break
    /// past `regions::NOT_YET_STARVED_HOURS` — adef5ddf's rule, which is
    /// all the record can say of a car that never said what it waits on.
    Undeclared(NotYetStreak),
    /// The car declared what it waits on, its `seen` check found that
    /// in the record, and the probe STILL said not-yet.
    SeenWhileNotYet { on: String, seen_at: String },
}

/// Is this car's not-yet ours to read? One definition, read by the
/// shed and by `boss orient`. A DECLARED wait not yet seen is never
/// starved, however long — its patience is stated, and it becomes ours
/// the moment the record holds what it named.
pub fn starved(md: &Value) -> Option<Starved> {
    let streak = not_yet_streak(md)?;
    match waits_on(md) {
        None => (streak.hours >= crate::regions::NOT_YET_STARVED_HOURS)
            .then_some(Starved::Undeclared(streak)),
        Some(w) => md
            .pointer(&format!("/proof_attempt/{WAITS_ON_SEEN_AT}"))
            .and_then(Value::as_str)
            .map(|seen_at| Starved::SeenWhileNotYet {
                on: w.on,
                seen_at: seen_at.to_string(),
            }),
    }
}

/// WHOSE MOVE A DECLARED WAIT IS (backlog 3881f5c9). Inside `waits_on`:
/// `"world"` for an event nobody here can cause, or the id of the actor
/// whose act it is. DECLARED, never inferred — `on` prose that says
/// "David opens it" names no owner, because a reader that guessed an
/// owner out of prose would be the mostly-sure the shed exists to
/// refuse. No writer sets it yet (`boss car waits-on` writes only `on`
/// and `seen`); until one does, it is written through the metadata
/// PATCH, and a car without it reads as ours.
pub const WAITS_ON_OWNER: &str = "owner";
/// Inside `waits_on`, optional: how many hours from the car's opening
/// the owner's move may take before the wait is ours again. A positive
/// integer or nothing — patience is the car's to state, not a default.
pub const WAITS_ON_MAX_WAIT_HOURS: &str = "max_wait_hours";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WaitOwner {
    World,
    Actor(String),
}

impl std::fmt::Display for WaitOwner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WaitOwner::World => f.write_str("the world"),
            WaitOwner::Actor(a) => f.write_str(a),
        }
    }
}

/// The owner a car's `waits_on` declares, or `None` for none or blank.
pub fn wait_owner(md: &Value) -> Option<WaitOwner> {
    let owner = non_blank(md.get(WAITS_ON)?.get(WAITS_ON_OWNER))?;
    Some(if owner.eq_ignore_ascii_case("world") {
        WaitOwner::World
    } else {
        WaitOwner::Actor(owner)
    })
}

/// A wait that is someone else's move.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnedWait {
    pub owner: WaitOwner,
    pub on: String,
    pub max_wait_hours: Option<i64>,
}

impl OwnedWait {
    /// Past the patience the car declared, measured on its age.
    pub fn overdue(&self, age_hours: i64) -> bool {
        self.max_wait_hours.is_some_and(|max| age_hours > max)
    }
}

/// Is this car's wait someone else's move? Only when all four hold: it
/// DECLARED the wait, it names an OWNER, a WORLD wait has something
/// that OBSERVES it (a `seen` check), and the event has not been seen
/// while the probe says not-yet ([`starved`] is `None`). Anything short
/// of that is ours — the shed's troubled colour means exactly that set.
///
/// WHY ONLY THE WORLD NEEDS AN OBSERVER (3881f5c9, fix shape (2)).
/// Nothing but a `seen` check can say a Stripe charge arrived, so an
/// unobserved world wait is ours to observe. A named actor's act is
/// that actor's next move whether or not a check watches for it — the
/// dev-door car, waiting on David's SSH CA ceremony with no probe at
/// all, troubled the shed alone on 2026-09-24 though its owner was
/// declared as data. Its bound is the patience the car declares, and a
/// check that does exist still turns a seen-while-not-yet into ours.
pub fn owned_wait(md: &Value) -> Option<OwnedWait> {
    let w = waits_on(md)?;
    let owner = wait_owner(md)?;
    if owner == WaitOwner::World && w.seen.is_none() {
        return None;
    }
    if starved(md).is_some() {
        return None;
    }
    Some(OwnedWait {
        owner,
        on: w.on,
        max_wait_hours: md
            .pointer(&format!("/{WAITS_ON}/{WAITS_ON_MAX_WAIT_HOURS}"))
            .and_then(Value::as_i64)
            .filter(|h| *h > 0),
    })
}

/// THE PROOF A LANDED CAR OWES (backlog b9005734, approved by David
/// 2026-09-24). The packet measured 21 cars standing at `proven`,
/// several five to seven days old, each waiting on a rare event nobody
/// here causes — a Stripe charge, a release David cuts, his destructive
/// prune, a red nightly crawl — and each holding its item open while the
/// shed read TROUBLED every hour of every day. A permanently red surface
/// is read like a silent one (CLAUDE.md §Diagnosis). So a car whose
/// wait is declared, observed and owned closes LANDED with its proof
/// owed, and the proof becomes an obligation keyed to the event it
/// waits on: when that event fires, the recorded probe runs again.
///
/// `proof_owed = "true"` is the one marker — a string, because a
/// protocol predicate compares strings (`job.metadata.proof_owed =
/// "true"`, the idiom every `abandoned` terminal uses) — and the
/// obligation reads it through [`owes_proof`]. Paying the proof removes
/// it; the car's closing outcome keeps the history.
pub const PROOF_OWED: &str = "proof_owed";

/// Does this car owe its proof? Only under the one marker.
pub fn owes_proof(md: &Value) -> bool {
    md.get(PROOF_OWED).and_then(Value::as_str) == Some("true")
}

/// Inside `waits_on`: the event that settles the wait, declared so a
/// MACHINE can match it — `{"closes": <kind>, "title": <prefix>}`, the
/// close of a packet of that kind, optionally narrowed by the start of
/// its title (an `ops-request`'s title leads with its verb). `on` is
/// prose for a reader and `seen` a probe for the forge; neither can key
/// a dispatcher firing, which is what an owed proof needs (b9005734).
/// The close marker carries `kind` and `title` on all three emit sites,
/// so this is matched against the event itself, never a re-fetch.
pub const WAITS_ON_EVENT: &str = "event";

/// A declared wait's event, read back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WaitEvent {
    /// The kind of packet whose close is the event.
    pub closes: String,
    /// Optional: the closing packet's title must start with this.
    pub title: Option<String>,
}

impl WaitEvent {
    /// Is this `jobs.job.closed` marker the declared event? The kind
    /// must match, and a declared prefix must lead the closing title —
    /// a marker with no title satisfies no prefix.
    pub fn fired_by(&self, closed: &Value) -> bool {
        let title = closed.get("title").and_then(Value::as_str);
        closed.get("kind").and_then(Value::as_str) == Some(self.closes.as_str())
            && self
                .title
                .as_deref()
                .is_none_or(|p| title.is_some_and(|t| t.starts_with(p)))
    }
}

/// The event a car's `waits_on` declares, or `None` — for no wait, a
/// wait naming nothing ([`waits_on`] refuses a blank `on`), or an event
/// whose `closes` is blank, so an empty object can never key an
/// obligation to every close there is.
pub fn wait_event(md: &Value) -> Option<WaitEvent> {
    waits_on(md)?;
    let e = md.get(WAITS_ON)?.get(WAITS_ON_EVENT)?;
    Some(WaitEvent {
        closes: non_blank(e.get("closes"))?,
        title: non_blank(e.get("title")),
    })
}

/// THE ITEM A CAR ANSWERS, AND AUTHORISES THE CLOSE OF.
///
/// The declared one-to-one job edge (`('ship-a-change', 'backlog_item',
/// 'job_id')`) the arrival rule `complete-feedback-branch-on-car-merged`
/// follows to complete the item's `build` step when the change merges —
/// so writing it is what closes the item, and the id is ref-checked at
/// the write.
///
/// ONE DEFINITION, LATE (backlog 973d353f). Its two siblings below were
/// `pub const` from the day each was written, while the commonest of the
/// three — 187 of 200 recent cars carried it, against 6 and 7 — stayed a
/// bare string in fourteen files, several of them lines that name
/// [`PARTIAL_ITEM`] by constant and this one by literal. It had not
/// drifted; the asymmetry was the hazard on its own, because a reader
/// who finds two of three as constants concludes the pattern is
/// constants and never greps for the third (CLAUDE.md §9a: a pin is a
/// holding action, the collapse is the destination).
pub const BACKLOG_ITEM: &str = "backlog_item";

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

/// EVERY OTHER ITEM A CAR ANSWERS, AND AUTHORISES THE CLOSE OF.
///
/// A `job_id_list` edge beside [`BACKLOG_ITEM`], followed on merge by
/// the same arrival rule (its `also_link`), so each listed item is
/// routed and built exactly as the primary is. `backlog_item` holds ONE
/// id, and on 2026-09-23 two landed items stayed open that way and were
/// dispatched to builders who rediscovered the landing: 5994de6d (its
/// fix rode on cars filed under three other items) and cab50f4c (car
/// c842f18b named only 3ec04168). Backlog a994f533.
pub const ALSO_ANSWERS: &str = "also_answers";
/// Why this car names no item at all — the answer a deliberately
/// item-less car gives (`--park-no-item`). Item-less cars legitimately
/// exist (a fix asked for in conversation, a defect found while
/// building something else); the reason is what lets a later reader
/// tell one from a car whose builder simply forgot, which is the
/// omission e1325456 measured thirteen times in nineteen.
pub const NO_ITEM_REASON: &str = "no_item_reason";

/// THE ORDERING EDGE A CAR DECLARES: the car this one must land BEHIND.
///
/// A declared job edge (`('ship-a-change', 'boards_after', 'job_id')`,
/// migration 20260911-a-car-declares-what-it-boards-after), so the id is
/// ref-checked and prefix-normalised at the write the way `backlog_item`
/// is — a mistyped car is refused, not silently pointed at nothing.
///
/// WHY AN EDGE AND NOT A HOLD. Four cars were held by hand on
/// 2026-09-10 and every one of them was an ordering constraint:
/// `fix/the-reclaim-follows-the-build` had to be gated `--hold` and
/// parked by hand purely so it would board solo, and
/// `feat/a-deleted-manifest-leaves-no-object` spent a day waiting for a
/// human to notice both halves of a two-part sequence were satisfied.
/// An edge states the constraint once, in the one place a car's other
/// facts are already stated, and the dock enforces it every 60 seconds
/// without anyone watching. `--hold` stays: a declared edge and a human
/// brake are different tools (design doc 364f892e, backlog d3320278).
///
/// Two READERS, one judgement: `boss train board` filters its candidates
/// on it, and the dock region (`regions::dock_edges`) counts the cars it
/// holds — both through [`boards_after_outcome`] below, which names which
/// of four situations holds (backlog 4142d821). The key lives here
/// because the writers (`boss gate --park-after`, the auto-park handler)
/// and those readers must not keep two spellings of one fact (CLAUDE.md
/// §9a).
pub const BOARDS_AFTER: &str = "boards_after";

/// The gate-run key `boss gate --park-after` stamps, which the auto-park
/// handler reads and copies onto the car as [`BOARDS_AFTER`].
///
/// IN CORE, NOT SPELLED TWICE. The writer is the CLI and the reader is a
/// dispatcher handler, in two crates that cannot import each other; every
/// other `park_*` key is a string literal in both places, agreeing by
/// coincidence. One `pub const` cannot drift from itself (CLAUDE.md §9a),
/// and a typo on either side would be a car filed with an ordering
/// constraint the dock never sees — a hold the builder believes in and
/// nothing enforces.
pub const PARK_BOARDS_AFTER: &str = "park_boards_after";

/// THE REST OF THE `park_*` FAMILY, here for the reason the one above
/// already gave: the writer is the CLI and the reader is a dispatcher
/// handler, in two crates that cannot import each other, so a key
/// spelled in both places agrees only by coincidence.
///
/// Until 2026-09-21 that was the state of all ten (backlog ef9a602d).
/// Five had no constant at all; five more had one in `boss-cli` — a
/// const on the WRITE side and a bare literal on the READ side, which
/// is the same coincidence wearing a better name. `PARK_BOARDS_AFTER`
/// was the only key both sides took from one definition.
///
/// THE FAILURE IS SILENT, which is why it is worth ten constants. A
/// mistyped key on the write side stamps a field nobody reads; on the
/// read side it reads a field nobody stamped. Either way the car
/// parks, the gate is green, and the receipt is quietly short a piece
/// — and for `PARK_BACKLOG_ITEM` that piece is what attaches a landed
/// fix to the packet it answers, so losing it leaves an item open
/// after its fix is in production, which is the residue the startup
/// protocol sends the next session to re-derive by hand.
pub const PARK_SUMMARY: &str = "park_summary";
pub const PARK_EXCLUDES: &str = "park_excludes";
pub const PARK_TEST: &str = "park_test";
pub const PARK_VERIFIED: &str = "park_verified";
pub const PARK_BACKLOG_ITEM: &str = "park_backlog_item";
pub const PARK_PROBE: &str = "park_probe";
pub const PARK_EXPECT: &str = "park_expect";
pub const PARK_PROOF_EVENT: &str = "park_proof_event";
pub const PARK_NO_ITEM: &str = "park_no_item";
pub const PARK_PARTIAL_ITEM: &str = "park_partial_item";
/// `boss gate --park-waits-on` (backlog e9b164a1 piece 3): the car's
/// declared wait as the builder states it at park — an object carrying
/// only the [`WAITS_ON`] fields given. The auto-park handler MERGES it
/// onto the car's `waits_on` with [`merge_waits_on`], never replaces.
pub const PARK_WAITS_ON: &str = "park_waits_on";
/// `boss gate --park-also-answers` (backlog a994f533, the writer half):
/// every OTHER item the car answers, as a JSON list of full ids. The
/// auto-park handler copies it onto the car as [`ALSO_ANSWERS`] through
/// [`also_answers`]. Until this existed the edge had a reader and no
/// writer, so nothing a builder typed could set it.
pub const PARK_ALSO_ANSWERS: &str = "park_also_answers";

/// The `also_answers` edge a car carries, read off what the gate stamped
/// under [`PARK_ALSO_ANSWERS`]: each id once, in the order given, blanks
/// dropped. Anything but a list, or a list with no id in it, is omitted
/// rather than written empty — the same absent-never-nulled contract as
/// [`item_provenance`], so a re-gate that names none leaves a recorded
/// list alone.
pub fn also_answers(stamped: Option<&Value>) -> serde_json::Map<String, Value> {
    let ids = stamped
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .fold(Vec::<&str>::new(), |mut seen, id| {
                    if !seen.contains(&id) {
                        seen.push(id);
                    }
                    seen
                })
        })
        .unwrap_or_default();
    let mut m = serde_json::Map::new();
    if !ids.is_empty() {
        m.insert(ALSO_ANSWERS.to_string(), json!(ids));
    }
    m
}

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

/// The SET of tiers a change touched (`infra/platform/tiers.toml`,
/// design 01c3cc3f), sorted, stamped on the gate-run by `boss gate`
/// beside `delivery_channel` and carried onto the car — a sorted JSON
/// array of tier names, empty for a change no tier claims.
pub const SOFTWARE_TIERS: &str = "software_tiers";
/// The headline among [`SOFTWARE_TIERS`]: the lowest-ranked tier the
/// change touched (core wins over frontend), the way `delivery_channel`
/// is the heaviest of a mixed car's paths. Absent when the set is empty.
pub const SOFTWARE_TIER: &str = "software_tier";

/// The tier stamps a gate-run carries, copied VERBATIM for the car —
/// the same copy-don't-rebuild rule the receipt and the proof intent
/// live by (ba429e7f, 2026-09-19). A set that is not an array, or a
/// headline that is not a non-empty string, is omitted (never nulled:
/// the metadata door deletes a null key, and a re-gate that classified
/// nothing must not strip what the first park recorded).
pub fn tier_stamps(md: &serde_json::Map<String, Value>) -> serde_json::Map<String, Value> {
    let mut m = serde_json::Map::new();
    if let Some(set) = md.get(SOFTWARE_TIERS).filter(|v| v.is_array()) {
        m.insert(SOFTWARE_TIERS.to_string(), set.clone());
    }
    if let Some(head) = md
        .get(SOFTWARE_TIER)
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
    {
        m.insert(SOFTWARE_TIER.to_string(), json!(head));
    }
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

/// The prose a re-gate carries onto a parked car, beside the receipt it
/// supersedes.
///
/// A car's account of itself — summary, excludes, test, verified — is
/// stamped on its scope/build/gate steps at the first park, and a
/// completed step is frozen. So until 2026-09-16 (c30e6276) a re-gate
/// wrote the fresh receipt, note and proof onto the job and left the
/// prose at the first build's words: a rebuilt car described what it
/// USED to do, to the yard, the train and the operator. The same rule
/// the receipt lives by applies — the frozen step stays as the first
/// head's record, and the current account rides the JOB under
/// `regate_*`, verbatim, where `regate_receipt` already is. Empty
/// prose is omitted, never nulled: the metadata door deletes a null
/// key, and a re-gate that restated nothing must not strip what an
/// earlier one carried.
pub fn regate_prose(
    summary: &str,
    excludes: &str,
    test: &str,
    verified: &str,
) -> serde_json::Map<String, Value> {
    let mut m = serde_json::Map::new();
    for (key, text) in [
        ("regate_summary", summary),
        ("regate_excludes", excludes),
        ("regate_test", test),
        ("regate_verified", verified),
    ] {
        let text = text.trim();
        if !text.is_empty() {
            m.insert(key.to_string(), json!(text));
        }
    }
    m
}

/// Is this packet still open? Callers list `status=open`, so the field
/// is usually redundant — and a fixture without one must still answer —
/// but a list that also holds finished cars (the handler pages both)
/// must not read one of those as live.
///
/// BOTH terminal statuses, not just `closed`. The jobs API has two
/// (`triage_on_park` and `sweep_settled` both already say
/// `closed | cancelled`), and a cancelled car is as done as a closed
/// one: it cannot be boarded, parked onto, rerailed, or take a proof.
/// Reading one as live is the same defect this predicate exists to
/// prevent, one status over.
///
/// Public because `boss prove` asks it too: its read is deliberately
/// `kind=ship-a-change` with NO status filter (`--recheck` re-runs a
/// proof on a closed car), so it is the other caller holding a mixed
/// list. One definition for "is this car live", not a fourth copy of
/// the terminal-status test (CLAUDE.md §9a).
pub fn is_open(car: &Value) -> bool {
    !matches!(
        car.get("status").and_then(Value::as_str),
        Some("closed" | "cancelled")
    )
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

/// The kinds of linked packet a park may ROUTE — the same two the
/// merge-time route lists (`complete-feedback-branch-on-car-merged`
/// v4, `route = [...]`), so the park and the merge agree about whose
/// triage a car may complete.
///
/// `user-feedback` joined on 2026-09-14 (backlog a29c3687). Until then
/// a feedback packet's triage was "the filer's own routing decision"
/// and the park left it alone — but a car PARKED AGAINST the packet is
/// that decision already made, by whoever built and linked the car,
/// and the v4 merge route closes it on the same reasoning. Leaving the
/// hours between park and merge un-routed put David's 9827c699 on the
/// feedback board as nobody's decision for seventy minutes while its
/// car sat on the dock and then rode a train. Its triage vocabulary
/// (`reproduce|design|build|duplicate|needs-info|decline`,
/// infra/platform/workflows/user-feedback.toml) admits `build`; the
/// `build` step then opens by `ready_when` and the merge route
/// completes it. A feedback packet with no car parked against it never
/// reaches this function, so its triage stays a person's.
pub const TRIAGEABLE_KINDS: &[&str] = &["backlog-item", "user-feedback"];

/// The routing step on that kind. Its `spec_slug` is the lookup;
/// `find_step`'s title fallback is given the same string because this
/// step has no separate title a car author could rely on.
pub const TRIAGE_SLUG: &str = "triage";

/// The disposition a parked car states: this car is the item's build.
pub const DISPOSITION_BUILD: &str = "build";

/// PURE: the triage write parking this car owes the item it links — or
/// `None` when there is nothing for a park to state. The HTTP stays with
/// each caller (`boss park`, `boss car open` and the dispatcher's
/// auto-park handler); the DECISION lives here once.
///
/// THE SAME TWO WRITES AS EVERY OTHER CAR WRITE — a [`StepWrite`],
/// addressed through the ITEM's id: the route through the step merge
/// door, then a status-only PUT (backlog e39a9d2a, Stage 1). This was
/// one PUT of `{status, metadata}` whose metadata was the step's as the
/// park had READ it plus the two route keys; the PUT replaces metadata
/// wholesale, so a key written between that read and the PUT was dropped
/// by omission. The merge body holds the route alone, because the merge
/// keeps what the step already carries.
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
/// `design` / `stale` / `decline` (or feedback its filer sent to
/// `reproduce` / `needs-info`), a closed or cancelled packet, a packet
/// with no routing step, and a kind outside `TRIAGEABLE_KINDS` all
/// answer `None` — so a re-gate, a refresh or a redelivery writes
/// nothing, and no human's disposition is ever overwritten.
pub fn triage_on_park(item: &Value, car_id: &str, branch: &str) -> Option<StepWrite> {
    let kind = item.get("kind").and_then(Value::as_str)?;
    if !TRIAGEABLE_KINDS.contains(&kind) {
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
    // Belt to the status guard's braces: a disposition already written
    // is a decision already made, whatever the step's status says.
    if step
        .get("metadata")
        .and_then(|m| m.get("disposition"))
        .is_some()
    {
        return None;
    }
    let step_id = step.get("id").and_then(Value::as_str)?;
    Some(StepWrite::completing(
        step_id,
        TRIAGE_SLUG,
        json!({
            "disposition": DISPOSITION_BUILD,
            "evidence": park_triage_evidence(car_id, branch),
        }),
    ))
}

/// The `evidence` the routing step records at done, naming WHAT made
/// the decision — a reader of the packet should not have to go find
/// out why its route says `build`. Shaped as the merge route's own
/// sentence is (`shipped and proven: {branch} — {title} (car {car})`,
/// complete-feedback-branch-on-car-merged v4): the moment differs, the
/// form does not, so the two reads on one packet's history read as
/// one story. `backlog-item`'s triage REQUIRES this key at done;
/// `user-feedback`'s declares only `finding`, and an extra key is
/// carried, not refused — the same key the merge route writes there.
fn park_triage_evidence(car_id: &str, branch: &str) -> String {
    format!(
        "routed at park: {branch} is this packet's build (car {car_id}). \
         --park-backlog-item names the car as the build, so the route is stated when \
         the car is filed rather than left un-triaged until the car merges \
         (backlog ca76d8f9, a29c3687)."
    )
}

// ---------------------------------------------------------------------------
// THE DECLARED ORDERING EDGE, JUDGED — one answer for the conductor and the dock
//
// Moved here from `boss-cli/src/train/boarding.rs` (backlog 4142d821, design
// cf820810 Q7). Until then only the conductor asked whether a parked car's
// `boards_after` predecessor had landed, so the conductor refused the car
// every 60 seconds while the dock region counted it as boardable and said
// "the boarding depth is met, a train is due" — the sentence it says two
// minutes after a healthy departure. Two readers of one edge must not keep
// two answers to "can this car board" (CLAUDE.md §9a), so the judgement
// lives in core beside the key it reads, and both take it from here.
//
// The words below are the conductor's own and are unchanged by the move:
// four situations, told apart at a glance, because an operator reading them
// is deciding whether the pipeline is stuck (d3320278):
//
//   still in flight  — nobody does anything; it departs on its own
//   landed           — satisfied; the car boards (no refusal at all)
//   abandoned        — a human must break the edge; it will never clear
//   no such Job      — a human must fix the reference
//
// AND IT MUST NEVER FREEZE A LANDING: an edge that cannot be READ boards the
// car and says why (`BoardUnjudged`). The edge exists to stop a known
// collision, not to become a new way for the pipeline to stop.
// ---------------------------------------------------------------------------

/// The structured marker a boarding-edge hold leaves on its `left_behind`
/// entry, so the window's own refusal line is composed from DATA and not
/// from sniffing the reason string back apart.
pub const EDGE_HOLD: &str = "edge_hold";
/// The predecessor is still in flight — self-clearing, no action.
pub const EDGE_HOLD_WAITING: &str = "waiting";
/// The edge can never be satisfied as declared — a person must act.
pub const EDGE_HOLD_NEEDS_HUMAN: &str = "needs_human";

/// What a reader managed to learn about a car's declared predecessor.
/// `Unreadable` is a first-class answer, not an error: "I could not ask"
/// must be distinguishable from "it is not there".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Predecessor {
    /// The Job came back — as the jobs API serves it, with its `steps` —
    /// judged by this file's own predicates so no reader can disagree with
    /// the rest of the system about what "landed" means.
    Found(Value),
    /// The jobs API answered that there is no such Job.
    Absent,
    /// The read itself failed — a blip, an outage, a malformed body.
    Unreadable(String),
}

/// Why a car may not board on its declared edge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EdgeHold {
    /// The reason, journal and Job chip alike — ONE string, as every
    /// other skip reason is.
    pub reason: String,
    /// `EDGE_HOLD_WAITING` or `EDGE_HOLD_NEEDS_HUMAN`.
    pub kind: &'static str,
    /// The predecessor as a reader names it — its branch and id8 where
    /// the packet came back, the id8 alone where it did not. Structured
    /// so the dock can say "waiting behind X" without parsing `reason`.
    pub behind: String,
}

/// What boarding should do about a car's declared edge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EdgeOutcome {
    /// Board it: the edge is satisfied, or there is none.
    Board,
    /// Board it, and SAY why the edge could not be judged. Fail-open by
    /// design — see the section comment.
    BoardUnjudged(String),
    /// Leave it behind, with the reason named on it.
    Hold(EdgeHold),
}

/// The predecessor a car's METADATA declares, if it declares one. A blank
/// value is no declaration — the metadata door deletes a null key but a
/// `""` is a real stored value, and `jobs_clear_waiting` shows `""` is how
/// an edge gets cleared in practice.
pub fn boards_after_of(md: &Value) -> Option<String> {
    md.get(BOARDS_AFTER)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// The predecessor a car (a whole packet) declared, if it declared one.
pub fn declared_predecessor(car: &Value) -> Option<String> {
    car.get("metadata").and_then(boards_after_of)
}

/// The first eight characters of an id — the spelling every journal,
/// report and surface prints.
fn id8(id: &str) -> String {
    id.chars().take(8).collect()
}

/// How to name a predecessor in a refusal an operator reads: its BRANCH
/// where we have it, never a bare id (MEMORY: refer by protocol + title).
/// The id8 rides along so the packet is still findable.
fn predecessor_name(declared: &str, pred: Option<&Value>) -> String {
    let branch = pred
        .and_then(|p| p.pointer("/metadata/branch"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    if branch.is_empty() {
        format!("car {}", id8(declared))
    } else {
        format!("{branch} (car {})", id8(declared))
    }
}

/// Where a live predecessor actually is, so "still in flight" names a
/// place rather than asserting a mood. The three states are this file's
/// (`is_boarded` / `is_parked` / `is_building`), in the order a car
/// passes through them backwards.
fn in_flight_at(pred: &Value) -> String {
    if is_boarded(pred) {
        let train = pred
            .pointer("/metadata/train")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if train.is_empty() {
            "aboard a train".to_string()
        } else {
            format!("aboard train {}", id8(train))
        }
    } else if is_parked(pred) {
        "parked at the dock".to_string()
    } else if is_building(pred) {
        "still building".to_string()
    } else {
        "open".to_string()
    }
}

/// How a spent predecessor ended, read off the packet rather than
/// guessed, so the refusal says what the record says.
fn spent_as(pred: &Value) -> String {
    let status = pred
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("not open");
    match pred.pointer("/metadata/outcome").and_then(Value::as_str) {
        Some(o) if !o.is_empty() => format!("{status}, outcome '{o}'"),
        _ => format!("{status}, no landing recorded"),
    }
}

/// PURE: what boarding does about one car's declared edge.
///
/// The four situations, and the exact words each gets. They are written
/// to be told apart at a glance by an operator scanning the journal:
/// "STILL IN FLIGHT" carries "no action needed", and both unsatisfiable
/// cases carry "a human must". That distinction is the feature — not the
/// hold (David on d3320278: "the refusal's wording matters as much as its
/// existence").
pub fn boards_after_outcome(declared: &str, pred: &Predecessor) -> EdgeOutcome {
    match pred {
        // FAIL-OPEN, LOUDLY. A car that would have boarded yesterday must
        // not be held because the system of record blipped while the
        // conductor asked about its edge.
        Predecessor::Unreadable(cause) => EdgeOutcome::BoardUnjudged(format!(
            "boards after car {}, and that packet could not be read ({cause}) — boarding \
             anyway: an unreadable edge is not evidence of a collision, and holding the \
             dock on a read failure would stop every train",
            id8(declared)
        )),
        Predecessor::Absent => EdgeOutcome::Hold(EdgeHold {
            reason: format!(
                "boards after car {}, which DOES NOT EXIST — a human must fix \
                 metadata.{} on this car (the edge is ref-checked at the write, so this \
                 id was stored before the edge was declared, or with ref-checking off)",
                id8(declared),
                BOARDS_AFTER
            ),
            kind: EDGE_HOLD_NEEDS_HUMAN,
            behind: predecessor_name(declared, None),
        }),
        Predecessor::Found(p) if is_landed(p) => EdgeOutcome::Board,
        Predecessor::Found(p) if is_open(p) => EdgeOutcome::Hold(EdgeHold {
            reason: format!(
                "boards after {}, which is STILL IN FLIGHT ({}) — no action needed; this \
                 car boards on a later window once that one lands",
                predecessor_name(declared, Some(p)),
                in_flight_at(p)
            ),
            kind: EDGE_HOLD_WAITING,
            behind: predecessor_name(declared, Some(p)),
        }),
        Predecessor::Found(p) => EdgeOutcome::Hold(EdgeHold {
            reason: format!(
                "boards after {}, which was ABANDONED ({}) — the edge can never be \
                 satisfied; a human must clear metadata.{} on this car, or abandon it too",
                predecessor_name(declared, Some(p)),
                spent_as(p),
                BOARDS_AFTER
            ),
            kind: EDGE_HOLD_NEEDS_HUMAN,
            behind: predecessor_name(declared, Some(p)),
        }),
    }
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

    /// THE TIER STAMPS ARE COPIED, NOT REBUILT (ba429e7f): the set rides
    /// as the gate-run wrote it, an empty set included (a root-only
    /// change touched no tier, and that is a reading); a headline that
    /// is blank or a set that is not an array is left out, never nulled.
    #[test]
    fn tier_stamps_copy_the_set_and_headline_verbatim_and_omit_what_is_malformed() {
        let md = json!({
            "software_tiers": ["core", "frontend"],
            "software_tier": "core",
            "delivery_channel": "software",
        });
        let t = tier_stamps(md.as_object().unwrap());
        assert_eq!(t.len(), 2);
        assert_eq!(t[SOFTWARE_TIERS], json!(["core", "frontend"]));
        assert_eq!(t[SOFTWARE_TIER], "core");
        assert!(!t.contains_key("delivery_channel"));

        let empty = json!({ "software_tiers": [] });
        let t = tier_stamps(empty.as_object().unwrap());
        assert_eq!(t[SOFTWARE_TIERS], json!([]));
        assert!(!t.contains_key(SOFTWARE_TIER));

        let bad = json!({ "software_tiers": "core", "software_tier": " " });
        assert!(tier_stamps(bad.as_object().unwrap()).is_empty());
        assert!(tier_stamps(&serde_json::Map::new()).is_empty());
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
            !p.contains_key(BACKLOG_ITEM),
            "a partial edge must never write the key the arrival rule follows"
        );
        let n = item_provenance(None, Some("David asked for this in conversation"));
        assert_eq!(n.len(), 1);
        assert_eq!(n[NO_ITEM_REASON], "David asked for this in conversation");
        assert_ne!(PARTIAL_ITEM, BACKLOG_ITEM);
        assert_ne!(NO_ITEM_REASON, BACKLOG_ITEM);
    }

    /// EVERY OTHER ITEM, AS STAMPED (a994f533, the writer half). The
    /// gate stamps a list; the car gets the declared `also_answers` edge
    /// with blanks dropped and repeats folded, and nothing at all when
    /// the gate stated none — absent, never an empty list, so a re-gate
    /// that names none leaves a recorded list alone.
    #[test]
    fn also_answers_carries_every_stated_id_once_and_nothing_else() {
        assert!(also_answers(None).is_empty());
        assert!(also_answers(Some(&json!([]))).is_empty());
        assert!(also_answers(Some(&json!(["  ", ""]))).is_empty());
        assert!(
            also_answers(Some(&json!("5994de6d"))).is_empty(),
            "a bare string is not the list the gate stamps"
        );
        let a = also_answers(Some(&json!(["5994de6d", " ", "cab50f4c", "5994de6d"])));
        assert_eq!(a.len(), 1);
        assert_eq!(a[ALSO_ANSWERS], json!(["5994de6d", "cab50f4c"]));
        assert_ne!(PARK_ALSO_ANSWERS, ALSO_ANSWERS);
        assert_ne!(ALSO_ANSWERS, BACKLOG_ITEM);
    }

    #[test]
    fn the_car_body_carries_the_fields_the_api_demands() {
        let b = car_body(
            "feat/x",
            "A thing does the thing. And more.",
            None,
            None,
            "emp-owner",
        );
        assert!(
            b["metadata"].get("delivery_channel").is_none(),
            "no delivery_channel when None"
        );
        let d = car_body("feat/x", "A thing.", None, Some("data"), "emp-owner");
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
        assert_eq!(b["owner_id"], "emp-owner", "the owner is the one handed in");
        assert!(b["metadata"].get(BACKLOG_ITEM).is_none());
    }

    /// THE THIRD KEY IS A CONSTANT TOO (backlog 973d353f). Three keys
    /// answer one question — which item this car answered — and
    /// `require_item_answer` guarantees exactly one of them is present.
    /// [`PARTIAL_ITEM`] and [`NO_ITEM_REASON`] were `pub const` from the
    /// day they were written; the third, and the commonest (187 of 200
    /// recent cars carried it, against 6 and 7), was a bare string in
    /// fourteen files. It had not drifted, which is why it was cheap to
    /// collapse — but the asymmetry was itself the hazard: a reader who
    /// finds two of three as constants concludes the pattern is
    /// constants, and will not grep for a literal spelling of the third.
    #[test]
    fn the_three_item_answer_keys_are_each_one_definition() {
        assert_eq!(BACKLOG_ITEM, "backlog_item", "the wire spelling is fixed");
        assert_ne!(BACKLOG_ITEM, PARTIAL_ITEM);
        assert_ne!(BACKLOG_ITEM, NO_ITEM_REASON);
        // The closing edge is the one the arrival rule follows, so the
        // provenance-only builder must still never write it.
        assert!(!item_provenance(Some("cf0f5e2d"), None).contains_key(BACKLOG_ITEM));
        let b = car_body("feat/x", "Summary", Some("de6f0c06"), None, "emp-owner");
        assert_eq!(b["metadata"][BACKLOG_ITEM], "de6f0c06");
    }

    #[test]
    fn a_backlog_edge_is_carried_when_given() {
        let b = car_body(
            "feat/x",
            "Summary",
            Some("de6f0c06-a341-4445-9f47-399dc27a60fb"),
            None,
            "emp-owner",
        );
        assert_eq!(
            b["metadata"][BACKLOG_ITEM],
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

    /// The re-gate's prose rides the job under `regate_*`, trimmed,
    /// and an empty field is absent rather than null — a null key is
    /// deleted by the metadata door.
    #[test]
    fn the_regate_prose_rides_the_job_and_omits_what_was_not_said() {
        let m = regate_prose(" rebuilt: now does X ", "not Y", "", "   ");
        assert_eq!(m["regate_summary"], "rebuilt: now does X");
        assert_eq!(m["regate_excludes"], "not Y");
        assert!(!m.contains_key("regate_test"), "{m:?}");
        assert!(!m.contains_key("regate_verified"), "{m:?}");
        assert!(regate_prose("", "", "", "").is_empty());
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

    /// A CANCELLED CAR IS NOT LIVE EITHER. The jobs API has two terminal
    /// statuses and `is_open` read only one, so an abandoned twin a
    /// person cancelled — rather than closed — still answered "this
    /// branch has a live car", which is the 02165b1d defect with the
    /// other terminal word. Asserted on both shapes, because `is_open`
    /// gates the boarded lookup and the parked one separately.
    #[test]
    fn a_cancelled_car_is_not_a_live_car() {
        let mut cancelled = twin();
        cancelled["status"] = json!("cancelled");
        assert!(!is_open(&cancelled));
        assert!(open_car_for(&[cancelled.clone()], BRANCH).is_none());
        let mut cancelled_aboard = boarded();
        cancelled_aboard["status"] = json!("cancelled");
        assert!(!is_boarded(&cancelled_aboard));
        assert!(open_car_for(&[cancelled_aboard], BRANCH).is_none());
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
        assert_eq!(w.status_body, json!({"status": "completed"}));
        assert_eq!(w.metadata["disposition"], DISPOSITION_BUILD);
        let evidence = w.metadata["evidence"].as_str().unwrap_or_default();
        assert!(
            evidence.contains("9442139b") && evidence.contains(BRANCH),
            "the evidence names the car and its branch: {evidence}"
        );
    }

    /// THE ROUTE RIDES THE MERGE DOOR (backlog e39a9d2a, Stage 1). This
    /// was one PUT of `{status, metadata}` whose metadata was the step's
    /// metadata AS THE PARK READ IT plus the two route keys. The step PUT
    /// replaces metadata wholesale, so any key written between that read
    /// and the PUT — a claim's lease, a person's note — was dropped by
    /// omission, silently. Now the two keys go through
    /// `PATCH …/steps/{id}/metadata`, one transaction against the row as
    /// it stands, and the PUT carries the status and nothing to drop. The
    /// step's own keys are therefore NOT in the body: the merge keeps
    /// them where they are, and a body that re-sent them would re-send a
    /// stale copy.
    #[test]
    fn the_park_route_merges_its_keys_and_puts_only_the_status() {
        for item in [untriaged_item(), untriaged_feedback()] {
            let w = triage_on_park(&item, CAR_ID, BRANCH).expect("an un-triaged packet routes");
            assert_eq!(
                w.status_body,
                json!({"status": "completed"}),
                "the PUT must carry the status alone"
            );
            let keys: Vec<&str> = w
                .metadata
                .as_object()
                .map(|m| m.keys().map(String::as_str).collect())
                .unwrap_or_default();
            assert_eq!(
                keys,
                ["disposition", "evidence"],
                "the merge body carries the route and nothing read off the step"
            );
            assert_eq!(
                w.merge_path("item-1"),
                format!("/api/jobs/item-1/steps/{}/metadata", w.step_id)
            );
            assert_eq!(
                w.status_path("item-1"),
                format!("/api/jobs/item-1/steps/{}", w.step_id)
            );
        }
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

    /// A `user-feedback` packet as the chrome bar files it: submitted,
    /// un-triaged, every branch still pending. Its triage vocabulary is
    /// `reproduce|design|build|duplicate|needs-info|decline`
    /// (infra/platform/workflows/user-feedback.toml), so `build` is a
    /// route it admits.
    fn untriaged_feedback() -> Value {
        json!({
            "id": "9827c699-3e49-4494-a812-d3ab5fa4bd69",
            "kind": "user-feedback",
            "status": "open",
            "steps": [
                { "id": "s-submitted", "spec_slug": "submitted", "status": "completed", "metadata": {} },
                { "id": "s-triage", "spec_slug": "triage", "status": "ready",
                  "metadata": { "finding": "a page for the codebase stats" } },
                { "id": "s-build", "spec_slug": "build", "status": "pending", "metadata": {} },
            ],
        })
    }

    /// A car parked against a user-feedback packet states its route at
    /// PARK time, not at merge (backlog a29c3687). Until this test the
    /// park covered `backlog-item` only and the packet sat un-triaged
    /// until the v4 merge route closed it — David's 9827c699 read as
    /// nobody's decision for seventy minutes with its car on the dock.
    #[test]
    fn parking_a_car_against_untriaged_feedback_routes_it_to_build() {
        let w = triage_on_park(&untriaged_feedback(), CAR_ID, BRANCH)
            .expect("un-triaged feedback gets the route its car states");
        assert_eq!(w.step_id, "s-triage");
        assert_eq!(w.status_body, json!({"status": "completed"}));
        assert_eq!(w.metadata["disposition"], DISPOSITION_BUILD);
        let evidence = w.metadata["evidence"].as_str().unwrap_or_default();
        assert!(
            evidence.contains(CAR_ID) && evidence.contains(BRANCH),
            "the evidence names the car and its branch: {evidence}"
        );
    }

    /// Feedback a person already routed — to `reproduce`, `design`, or
    /// anything else — is a decision, and a park never overwrites it;
    /// nor does it touch a packet whose triage is done and whose open
    /// step is further along.
    #[test]
    fn feedback_already_past_triage_is_left_exactly_alone() {
        let mut investigating = untriaged_feedback();
        investigating["steps"][1]["status"] = json!("completed");
        investigating["steps"][1]["metadata"] = json!({ "disposition": "reproduce" });
        investigating["steps"][2] = json!({ "id": "s-investigate", "spec_slug": "investigate",
            "status": "ready", "metadata": {} });
        assert!(triage_on_park(&investigating, CAR_ID, BRANCH).is_none());

        let mut decided = untriaged_feedback();
        decided["steps"][1]["metadata"] = json!({ "disposition": "needs-info" });
        assert!(triage_on_park(&decided, CAR_ID, BRANCH).is_none());
    }

    /// The park routes the two kinds a car may be parked against and no
    /// other: a `design-doc`, an `ops-request`, a `ship-a-change` with a
    /// step that happens to be called `triage` is not a park's to decide.
    #[test]
    fn a_packet_of_any_other_kind_is_untouched() {
        for kind in ["design-doc", "ops-request", "ship-a-change", "gate-run"] {
            let mut other = untriaged_item();
            other["kind"] = json!(kind);
            assert!(triage_on_park(&other, CAR_ID, BRANCH).is_none(), "{kind}");
        }
        let mut kindless = untriaged_item();
        if let Some(m) = kindless.as_object_mut() {
            m.remove("kind");
        }
        assert!(triage_on_park(&kindless, CAR_ID, BRANCH).is_none());
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
        assert_eq!(writes[0].status_body, json!({"status": "completed"}));
        assert_eq!(writes[0].metadata["summary"], "does a thing");
        assert_eq!(writes[0].metadata["excludes"], "not that");
        assert_eq!(
            writes[0].metadata["completed_at"], "2026-09-10T18:00:00Z",
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
        assert_eq!(writes[0].metadata["test"], "ran the tests");
        assert_eq!(writes[1].step_id, "s-gate");
        assert_eq!(
            writes[1].metadata["receipt"], GREEN,
            "the receipt rides verbatim, as it does on a fresh car"
        );
        assert_eq!(writes[1].metadata["verified"], "seen working");
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
            assert_eq!(w.status_body, json!({"status": "completed"}));
            assert_eq!(
                w.metadata["completed_at"], "2026-09-10T18:30:00Z",
                "{} was filled without saying when",
                w.title
            );
        }
    }

    /// NO CAR WRITE PUTS METADATA (backlog e39a9d2a, car 2 of its plan).
    ///
    /// The step PUT REPLACES metadata wholesale, and the registry
    /// materializes keys onto every step at admission —
    /// `metadata_defaults`, `authority_role`, `station`, `audience`,
    /// `claimable` — so these writers, which build their bodies fresh with
    /// no read, shed every one of those keys on every `boss car open`,
    /// `boss park` and auto-park. The evidence rides the step MERGE door
    /// (`PATCH …/steps/{id}/metadata`, one transaction against the row as
    /// it stands, so it cannot race) and the completion is a PUT carrying
    /// the status and nothing else to drop.
    #[test]
    fn every_car_write_merges_its_evidence_and_puts_only_the_status() {
        let opened = open_writes(&fresh("f1", BRANCH), "s", "e", at("2026-09-10T18:00:00Z"))
            .expect("a fresh car can be opened");
        let finished = finish_writes(
            &fresh("f1", BRANCH),
            "s",
            "e",
            "t",
            "v",
            &receipt(),
            at("2026-09-10T18:30:00Z"),
        )
        .expect("a fresh car can be finished");
        assert!(!opened.is_empty() && finished.len() == 3);
        for w in opened.iter().chain(&finished) {
            assert_eq!(
                w.status_body,
                json!({"status": "completed"}),
                "{}: the PUT must carry the status alone — a metadata key in it \
                 replaces the step's stored keys wholesale",
                w.title
            );
            assert!(
                w.metadata.as_object().is_some_and(|m| !m.is_empty()),
                "{}: the evidence rides the merge body",
                w.title
            );
            assert!(
                w.metadata.get("status").is_none(),
                "{}: `status` on the merge door would be a metadata key named status",
                w.title
            );
            assert_eq!(
                w.merge_path("car-1"),
                format!("/api/jobs/car-1/steps/{}/metadata", w.step_id),
                "the merge door, addressed through the car the step is on"
            );
            assert_eq!(
                w.status_path("car-1"),
                format!("/api/jobs/car-1/steps/{}", w.step_id)
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

#[cfg(test)]
mod not_yet_streak_tests {
    use super::*;

    const PROBE: &str = "grep -c x f || exit 75";

    fn not_yet(at: &str, since: Option<&str>, runs: Option<u64>) -> Value {
        let mut a = json!({"at": at, "exit": 75, "not_yet": true, "probe": PROBE});
        if let Some(s) = since {
            a[NOT_YET_SINCE] = json!(s);
        }
        if let Some(n) = runs {
            a[NOT_YET_RUNS] = json!(n);
        }
        a
    }

    /// The first not-yet of a streak starts it at this run.
    #[test]
    fn a_first_not_yet_starts_the_streak_at_this_run() {
        let (since, runs) = carried_not_yet_streak(None, PROBE, "2026-09-23T07:00:00Z");
        assert_eq!((since.as_str(), runs), ("2026-09-23T07:00:00Z", 1));
    }

    /// A not-yet after a not-yet of the SAME probe keeps the streak's
    /// start and counts the run — the only way a reader can later tell
    /// "asked once" from "asked 86 times across four days".
    #[test]
    fn a_not_yet_after_a_not_yet_carries_the_start_and_counts() {
        let prior = not_yet(
            "2026-09-23T06:00:00Z",
            Some("2026-09-19T05:50:00Z"),
            Some(85),
        );
        let (since, runs) = carried_not_yet_streak(Some(&prior), PROBE, "2026-09-23T07:00:00Z");
        assert_eq!((since.as_str(), runs), ("2026-09-19T05:50:00Z", 86));
    }

    /// A record written before the streak existed still vouches for its
    /// own run: its `at` is the earliest not-yet this door can prove, so
    /// the streak starts there rather than at zero on every car already
    /// standing in the shed when this lands.
    #[test]
    fn a_legacy_not_yet_record_dates_the_streak_from_its_own_run() {
        let prior = not_yet("2026-09-23T06:00:00Z", None, None);
        let (since, runs) = carried_not_yet_streak(Some(&prior), PROBE, "2026-09-23T07:00:00Z");
        assert_eq!((since.as_str(), runs), ("2026-09-23T06:00:00Z", 2));
    }

    /// THE STREAK BELONGS TO THE PROBE TEXT. A corrected probe is the
    /// repair for a starved one (52e0287e), and inheriting the old
    /// probe's four days would name the repair starved on its first run.
    /// And a run that answered anything but not-yet ends the streak.
    #[test]
    fn a_new_probe_or_a_different_answer_restarts_the_streak() {
        let prior = not_yet(
            "2026-09-23T06:00:00Z",
            Some("2026-09-19T05:50:00Z"),
            Some(85),
        );
        let (since, runs) =
            carried_not_yet_streak(Some(&prior), "a corrected probe", "2026-09-23T07:00:00Z");
        assert_eq!((since.as_str(), runs), ("2026-09-23T07:00:00Z", 1));

        let failed =
            json!({"at": "2026-09-23T06:00:00Z", "exit": 1, "not_yet": false, "probe": PROBE});
        let (since, runs) = carried_not_yet_streak(Some(&failed), PROBE, "2026-09-23T07:00:00Z");
        assert_eq!((since.as_str(), runs), ("2026-09-23T07:00:00Z", 1));
    }

    /// The reader: the streak's length is measured between its first and
    /// its latest not-yet — two runs that happened — never against the
    /// clock, so a recheck that stopped running cannot age a streak.
    #[test]
    fn the_streak_reads_back_as_hours_between_its_first_and_latest_run() {
        let md = json!({
            PROOF_PROBE: PROBE,
            "proof_attempt": not_yet("2026-09-23T07:00:00Z", Some("2026-09-19T05:50:00Z"), Some(86)),
        });
        assert_eq!(
            not_yet_streak(&md),
            Some(NotYetStreak {
                hours: 97,
                runs: 86
            })
        );
        // Legacy: one run vouched for, no length yet.
        let md = json!({
            PROOF_PROBE: PROBE,
            "proof_attempt": not_yet("2026-09-23T07:00:00Z", None, None),
        });
        assert_eq!(
            not_yet_streak(&md),
            Some(NotYetStreak { hours: 0, runs: 1 })
        );
    }

    /// No streak when the last run did not say not-yet, and none when the
    /// car's recorded probe is no longer the one that ran — a probe
    /// corrected by a metadata PATCH stops reading as starved at once,
    /// not an hour later when the recheck next writes.
    #[test]
    fn no_streak_for_another_answer_or_a_since_replaced_probe() {
        let failed = json!({
            PROOF_PROBE: PROBE,
            "proof_attempt": {"at": "2026-09-23T07:00:00Z", "exit": 1, "probe": PROBE},
        });
        assert_eq!(not_yet_streak(&failed), None);
        let replaced = json!({
            PROOF_PROBE: "a corrected probe",
            "proof_attempt": not_yet("2026-09-23T07:00:00Z", Some("2026-09-19T05:50:00Z"), Some(86)),
        });
        assert_eq!(not_yet_streak(&replaced), None);
        assert_eq!(not_yet_streak(&json!({})), None);
    }
}

#[cfg(test)]
mod waits_on_tests {
    use super::*;

    const PROBE: &str = "grep -c x f || exit 75";

    /// A car at `proven` whose probe has said not-yet for `hours` straight.
    fn waiting(hours: i64, waits: Option<Value>, seen_at: Option<&str>) -> Value {
        let last = chrono::DateTime::parse_from_rfc3339("2026-09-26T12:00:00Z").unwrap();
        let since = (last - chrono::Duration::hours(hours)).to_rfc3339();
        let mut attempt = json!({
            "at": last.to_rfc3339(), "exit": 75, "not_yet": true, "probe": PROBE,
            NOT_YET_SINCE: since, NOT_YET_RUNS: hours + 1,
        });
        if let Some(s) = seen_at {
            attempt[WAITS_ON_SEEN_AT] = json!(s);
        }
        let mut md = json!({PROOF_PROBE: PROBE, "proof_attempt": attempt});
        if let Some(w) = waits {
            md[WAITS_ON] = w;
        }
        md
    }

    /// THE WRITER MERGES, IT DOES NOT REPLACE (backlog e9b164a1 piece 3).
    /// Until this, `boss car waits-on` PATCHed the whole object, so a
    /// re-statement of `on` or `seen` dropped an `owner` an operator had
    /// added by hand — and a wait without an owner reads as ours. Each
    /// field the update carries replaces that field; every other field
    /// the car already declares is kept; a result naming no `on` declares
    /// nothing and is `None`.
    #[test]
    fn a_written_declaration_merges_into_the_one_the_car_carries() {
        let owned = json!({"on": "a release", "seen": "true", WAITS_ON_OWNER: "emp-david"});
        let got = merge_waits_on(
            Some(&owned),
            &json!({"on": "a tagged release", "seen": "exit 0"}),
        );
        assert_eq!(
            got,
            Some(json!({"on": "a tagged release", "seen": "exit 0", WAITS_ON_OWNER: "emp-david"}))
        );
        // An owner and a patience added to a declaration keep its on/seen.
        let got = merge_waits_on(
            Some(&waits_on_value("a Stripe charge", Some("true"))),
            &json!({WAITS_ON_OWNER: "world", WAITS_ON_MAX_WAIT_HOURS: 336}),
        )
        .unwrap();
        let md = json!({ WAITS_ON: got });
        assert_eq!(wait_owner(&md), Some(WaitOwner::World));
        assert_eq!(waits_on(&md).unwrap().seen.as_deref(), Some("true"));
        assert_eq!(md[WAITS_ON][WAITS_ON_MAX_WAIT_HOURS], 336);
        // Nothing to merge into and no `on`: nothing is declared.
        assert_eq!(
            merge_waits_on(None, &json!({WAITS_ON_OWNER: "world"})),
            None
        );
        // A non-object recorded by hand is replaced, not merged into.
        assert_eq!(
            merge_waits_on(Some(&json!("prose only")), &json!({"on": "x"})),
            Some(json!({"on": "x"}))
        );
    }

    /// UNDECLARED: exactly adef5ddf's rule — past the bound it is ours to
    /// read, under it nobody's business yet.
    #[test]
    fn an_undeclared_wait_is_starved_only_past_the_bound() {
        assert!(matches!(
            starved(&waiting(97, None, None)),
            Some(Starved::Undeclared(NotYetStreak { hours: 97, .. }))
        ));
        assert_eq!(starved(&waiting(40, None, None)), None);
    }

    /// DECLARED AND NOT SEEN: the car said what it waits on and the
    /// record does not hold it yet, so however long the streak, the move
    /// is the world's (the six cars b461341d triaged, at 97h to 134h).
    #[test]
    fn a_declared_wait_not_yet_seen_is_never_starved() {
        let w = waits_on_value("a Stripe sponsorship charge", None);
        assert_eq!(starved(&waiting(134, Some(w), None)), None);
    }

    /// DECLARED AND SEEN, PROBE STILL NOT YET: the event the car named
    /// is in the record and the probe still cannot see it — the true
    /// "ours to read", at any streak length.
    #[test]
    fn a_declared_wait_seen_while_the_probe_says_not_yet_is_ours_at_once() {
        let w = waits_on_value("a red crawl", Some("true"));
        let md = waiting(3, Some(w), Some("2026-09-26T11:00:00Z"));
        assert_eq!(
            starved(&md),
            Some(Starved::SeenWhileNotYet {
                on: "a red crawl".into(),
                seen_at: "2026-09-26T11:00:00Z".into(),
            })
        );
    }

    /// A declaration must name something: a blank `on` is no declaration,
    /// so it cannot silence the label by being present.
    #[test]
    fn a_blank_or_malformed_declaration_is_no_declaration() {
        assert_eq!(waits_on(&json!({WAITS_ON: {"on": "  "}})), None);
        assert_eq!(waits_on(&json!({WAITS_ON: "prose only"})), None);
        assert!(starved(&waiting(97, Some(json!({"on": ""})), None)).is_some());
        assert_eq!(
            waits_on(&json!({WAITS_ON: waits_on_value("x", Some(" "))})),
            Some(WaitsOn {
                on: "x".into(),
                seen: None
            })
        );
    }

    /// The first run that saw the event dates the sighting; later runs
    /// that still see it keep that date, and a run that does not see it
    /// clears it.
    #[test]
    fn a_sighting_is_dated_from_the_first_run_that_saw_it() {
        let prior = json!({WAITS_ON_SEEN_AT: "2026-09-26T09:00:00Z"});
        assert_eq!(
            carried_seen_at(Some(&prior), true, "2026-09-26T10:00:00Z").as_deref(),
            Some("2026-09-26T09:00:00Z")
        );
        assert_eq!(
            carried_seen_at(None, true, "2026-09-26T10:00:00Z").as_deref(),
            Some("2026-09-26T10:00:00Z")
        );
        assert_eq!(carried_seen_at(Some(&prior), false, "x"), None);
    }

    /// A declared wait with its owner and optional patience added.
    fn owned(on: &str, seen: Option<&str>, owner: Value, max: Value) -> Value {
        let mut w = waits_on_value(on, seen);
        w[WAITS_ON_OWNER] = owner;
        w[WAITS_ON_MAX_WAIT_HOURS] = max;
        w
    }

    /// THE OWNER IS READ, NEVER INFERRED (backlog 3881f5c9): `world`
    /// (any case) is the world, any other non-blank string is the actor
    /// named, and a blank or missing owner is no owner — the `on` prose
    /// saying "David opens it" does not make David the owner.
    #[test]
    fn a_wait_owner_is_the_declared_field_and_nothing_else() {
        let md =
            |owner: Value| json!({WAITS_ON: owned("an event", Some("true"), owner, Value::Null)});
        assert_eq!(wait_owner(&md(json!(" World "))), Some(WaitOwner::World));
        assert_eq!(
            wait_owner(&md(json!("emp-david"))),
            Some(WaitOwner::Actor("emp-david".into()))
        );
        assert_eq!(wait_owner(&md(json!("  "))), None);
        assert_eq!(wait_owner(&md(Value::Null)), None);
        let prose = json!({WAITS_ON: waits_on_value("a release (David opens it)", Some("true"))});
        assert_eq!(wait_owner(&prose), None);
    }

    /// OBSERVED, OWNED, NOT SEEN: someone else's move, and only that.
    /// Every one of the three missing — the owner, the `seen` check, or
    /// the event still unseen — leaves the wait ours.
    #[test]
    fn an_owned_wait_is_only_a_declared_observed_owned_unseen_one() {
        let w = owned("a Stripe charge", Some("true"), json!("world"), json!(336));
        let md = waiting(134, Some(w.clone()), None);
        assert_eq!(
            owned_wait(&md),
            Some(OwnedWait {
                owner: WaitOwner::World,
                on: "a Stripe charge".into(),
                max_wait_hours: Some(336),
            })
        );
        // Seen while not yet: ours, not the owner's.
        assert_eq!(
            owned_wait(&waiting(3, Some(w), Some("2026-09-26T11:00:00Z"))),
            None
        );
        // No `seen` check: nothing can say the event came.
        let unobserved = owned("a Stripe charge", None, json!("world"), Value::Null);
        assert_eq!(owned_wait(&waiting(134, Some(unobserved), None)), None);
        // No owner.
        let unowned = waits_on_value("a Stripe charge", Some("true"));
        assert_eq!(owned_wait(&waiting(134, Some(unowned), None)), None);
        // Undeclared.
        assert_eq!(owned_wait(&waiting(134, None, None)), None);
    }

    /// A NAMED ACTOR'S ACT NEEDS NO SEEN CHECK TO BE THEIRS (backlog
    /// 3881f5c9, fix shape (2)). Measured 2026-09-24 16:42Z: the shed
    /// read troubled on ONE car, the dev-door login, whose declared wait
    /// is David's Access SSH CA ceremony — `owner: emp-david`, no
    /// `seen`, no probe. The observation rule exists because nothing
    /// else can say a WORLD event arrived; an actor's act has its actor,
    /// whose next move it is, and the car's own declared patience still
    /// bounds it. A world wait with no `seen` stays ours, and an actor's
    /// wait whose check DID see the act while the probe says not yet is
    /// ours too — the declaration never hides a contradiction.
    #[test]
    fn a_named_actors_act_is_theirs_without_a_seen_check_and_the_worlds_is_not() {
        let act = owned("the SSH CA ceremony", None, json!("emp-david"), Value::Null);
        assert_eq!(
            owned_wait(&json!({ WAITS_ON: act })),
            Some(OwnedWait {
                owner: WaitOwner::Actor("emp-david".into()),
                on: "the SSH CA ceremony".into(),
                max_wait_hours: None,
            }),
            "declared on no probe at all, as the dev-door car is"
        );
        let world = owned("a Stripe charge", None, json!("world"), Value::Null);
        assert_eq!(owned_wait(&json!({ WAITS_ON: world })), None);
        let seen = owned("a release", Some("true"), json!("emp-david"), Value::Null);
        assert_eq!(
            owned_wait(&waiting(3, Some(seen), Some("2026-09-26T11:00:00Z"))),
            None
        );
    }

    /// PATIENCE IS OPTIONAL AND BOUNDED ONLY WHEN DECLARED: no
    /// `max_wait_hours` is never overdue; a declared one is overdue past
    /// it on the car's age; a zero, negative or non-numeric one is no
    /// declaration, so it cannot turn a car overdue the hour it lands.
    #[test]
    fn a_declared_max_wait_bounds_the_owned_wait_and_nothing_else_does() {
        let read = |max: Value| {
            let w = owned("a release", Some("true"), json!("emp-david"), max);
            owned_wait(&waiting(10, Some(w), None)).unwrap()
        };
        assert!(!read(Value::Null).overdue(10_000));
        assert!(read(json!(48)).overdue(49));
        assert!(!read(json!(48)).overdue(48));
        assert_eq!(read(json!(0)).max_wait_hours, None);
        assert_eq!(read(json!(-5)).max_wait_hours, None);
        assert_eq!(read(json!("48")).max_wait_hours, None);
    }

    /// A declared wait with the event a machine can match — the close of
    /// a packet of one kind, optionally narrowed by its title.
    fn with_event(event: Value) -> Value {
        let mut w = waits_on_value("a cut-a-release tag", Some("true"));
        w[WAITS_ON_EVENT] = event;
        json!({ WAITS_ON: w })
    }

    fn close_marker(kind: &str, title: &str) -> Value {
        json!({"id": "p1", "kind": kind, "title": title, "outcome": "answered",
               "closed_on": "2026-09-24", "subject_id": "forge", "parent_step_id": null})
    }

    /// THE EVENT IS DECLARED AS DATA, and read back only when it names
    /// the kind whose close it is. A blank or absent `closes`, a wait
    /// with no `on`, or no `event` at all is no declaration — so an
    /// empty object cannot key an obligation to every close there is.
    #[test]
    fn a_wait_event_is_read_only_when_it_names_the_closing_kind() {
        let md = with_event(json!({"closes": "ops-request", "title": "tag-release"}));
        assert_eq!(
            wait_event(&md),
            Some(WaitEvent {
                closes: "ops-request".into(),
                title: Some("tag-release".into()),
            })
        );
        let bare = with_event(json!({"closes": "maintenance-playground-crawl"}));
        assert_eq!(wait_event(&bare).and_then(|e| e.title), None);
        assert_eq!(wait_event(&with_event(json!({"closes": "  "}))), None);
        assert_eq!(wait_event(&with_event(json!({}))), None);
        assert_eq!(wait_event(&with_event(Value::Null)), None);
        let no_on = json!({ WAITS_ON: {"on": "", "event": {"closes": "ops-request"}} });
        assert_eq!(wait_event(&no_on), None);
        assert_eq!(wait_event(&json!({})), None);
    }

    /// FIRED BY exactly the close it names: the kind must match, and a
    /// declared title prefix must lead the closing packet's title. A
    /// marker with no title never satisfies a declared prefix.
    #[test]
    fn a_wait_event_is_fired_by_the_close_it_names_and_no_other() {
        let tag = wait_event(&with_event(
            json!({"closes": "ops-request", "title": "tag-release"}),
        ))
        .unwrap();
        assert!(tag.fired_by(&close_marker(
            "ops-request",
            "tag-release on forge — cut-a-release v0.4.0"
        )));
        assert!(!tag.fired_by(&close_marker(
            "ops-request",
            "converge on forge — a train merged"
        )));
        assert!(!tag.fired_by(&close_marker("cut-a-release", "tag-release v0.4.0")));
        assert!(!tag.fired_by(&json!({"kind": "ops-request", "title": null})));
        let crawl = wait_event(&with_event(
            json!({"closes": "maintenance-playground-crawl"}),
        ))
        .unwrap();
        assert!(crawl.fired_by(&close_marker("maintenance-playground-crawl", "anything")));
        assert!(!crawl.fired_by(&close_marker("maintenance-sweep", "anything")));
    }

    /// OWED IS ONE MARKER, spelled once: the string `"true"`. Anything
    /// else — absent, a boolean, `"paid"` — owes nothing, so a car whose
    /// proof was paid drops out of the obligation by the same read.
    #[test]
    fn a_car_owes_its_proof_only_under_the_one_marker() {
        assert!(owes_proof(&json!({ PROOF_OWED: "true" })));
        assert!(!owes_proof(&json!({ PROOF_OWED: true })));
        assert!(!owes_proof(&json!({ PROOF_OWED: "paid" })));
        assert!(!owes_proof(&json!({})));
    }
}

// ---------------------------------------------------------------------------
// The declared ordering edge — the four refusals, and the fail-open.
// ---------------------------------------------------------------------------

/// WHAT AN OPERATOR DEPENDS ON HERE IS THE WORDING, so the wording is
/// what these assert. David on d3320278: *"the refusal's wording matters
/// as much as its existence: the dock's no-departure line is read by an
/// operator deciding whether the pipeline is stuck, so 'held: boards
/// after <car>, which is abandoned' has to be distinguishable from
/// 'held: boards after <car>, still in flight' — the first needs a
/// human, the second does not."*
///
/// A test that only checked "it held" would let the four collapse into
/// one message a release later, which is the quiet hold the feature
/// exists to remove.
///
/// Moved from `boss-cli/src/train/boarding.rs` with the function they
/// pin (backlog 4142d821): the words are unchanged, and so are these.
#[cfg(test)]
mod boards_after_tests {
    use super::{
        EDGE_HOLD_NEEDS_HUMAN, EDGE_HOLD_WAITING, EdgeOutcome, Predecessor, boards_after_of,
        boards_after_outcome, declared_predecessor,
    };
    use serde_json::{Value, json};

    const PRED: &str = "bbbbbbbb-1111-2222-3333-444444444444";

    /// A predecessor packet: open, with a branch, and whatever extra
    /// metadata / steps the situation needs.
    fn pred(status: &str, md: Value, steps: Value) -> Value {
        let mut metadata = json!({"branch": "fix/the-predecessor"});
        if let (Some(dst), Some(src)) = (metadata.as_object_mut(), md.as_object()) {
            for (k, v) in src {
                dst.insert(k.clone(), v.clone());
            }
        }
        json!({"id": PRED, "status": status, "metadata": metadata, "steps": steps})
    }

    fn review(status: &str) -> Value {
        json!([{"spec_slug": "review", "status": status}])
    }

    fn hold_reason(declared: &str, p: &Predecessor) -> String {
        match boards_after_outcome(declared, p) {
            EdgeOutcome::Hold(h) => h.reason,
            other => panic!("expected a hold, got {other:?}"),
        }
    }

    /// (1) STILL IN FLIGHT — nobody needs to do anything, and the line
    /// says so outright. It also names WHERE the predecessor is, because
    /// "in flight" alone sends the reader to the yard to find out.
    #[test]
    fn a_predecessor_in_flight_holds_and_asks_for_nobody() {
        let p = Predecessor::Found(pred(
            "open",
            json!({"train": "77777777-aaaa-bbbb-cccc-dddddddddddd"}),
            review("ready"),
        ));
        let r = hold_reason(PRED, &p);
        assert!(
            r.contains("STILL IN FLIGHT") && r.contains("aboard train 77777777"),
            "it must name the state AND where: {r}"
        );
        assert!(
            r.contains("no action needed"),
            "an operator deciding whether the pipeline is stuck must be told it is not: {r}"
        );
        assert!(
            !r.contains("human"),
            "a self-clearing hold must never read as one that needs a person: {r}"
        );
        assert_eq!(
            match boards_after_outcome(PRED, &p) {
                EdgeOutcome::Hold(h) => h.kind,
                other => panic!("{other:?}"),
            },
            EDGE_HOLD_WAITING
        );
    }

    /// The dock and the build are in-flight states too, and each names
    /// itself — a car waiting on one still building is a different wait
    /// from one waiting on a car about to merge.
    #[test]
    fn in_flight_names_the_dock_and_the_build_separately() {
        let parked = hold_reason(
            PRED,
            &Predecessor::Found(pred("open", json!({}), review("ready"))),
        );
        assert!(parked.contains("parked at the dock"), "{parked}");
        let building = hold_reason(
            PRED,
            &Predecessor::Found(pred(
                "open",
                json!({}),
                json!([{"spec_slug": "gate", "status": "ready"}]),
            )),
        );
        assert!(building.contains("still building"), "{building}");
    }

    /// (2) LANDED — the edge is satisfied and the car boards. If this
    /// ever holds, the bug is in the filter and not on the dock.
    #[test]
    fn a_landed_predecessor_satisfies_the_edge() {
        for landed in [
            pred("closed", json!({"outcome": "merged"}), json!([])),
            pred("open", json!({"merged": "true"}), review("ready")),
        ] {
            assert_eq!(
                boards_after_outcome(PRED, &Predecessor::Found(landed.clone())),
                EdgeOutcome::Board,
                "a landed predecessor must board its successor: {landed}"
            );
        }
    }

    /// (3) ABANDONED — it can NEVER clear, so the line says a human must
    /// act, says what to do, and reports how the record says it ended.
    #[test]
    fn an_abandoned_predecessor_names_a_human_and_what_to_clear() {
        let p = Predecessor::Found(pred(
            "closed",
            json!({"outcome": "abandoned"}),
            review("ready"),
        ));
        let r = hold_reason(PRED, &p);
        assert!(r.contains("ABANDONED"), "{r}");
        assert!(
            r.contains("can never be satisfied"),
            "waiting is futile and the line must say so: {r}"
        );
        assert!(
            r.contains("a human must clear metadata.boards_after"),
            "name the fix, not just the fault: {r}"
        );
        assert!(
            r.contains("closed, outcome 'abandoned'"),
            "report what the record says, not a guess: {r}"
        );
        assert!(
            !r.contains("no action needed"),
            "this one DOES need action: {r}"
        );
        assert_eq!(
            match boards_after_outcome(PRED, &p) {
                EdgeOutcome::Hold(h) => h.kind,
                other => panic!("{other:?}"),
            },
            EDGE_HOLD_NEEDS_HUMAN
        );
    }

    /// A cancelled predecessor is spent, not landed — the same refusal,
    /// and it must not be read as in flight just because `outcome` is
    /// missing.
    #[test]
    fn a_cancelled_predecessor_is_spent_not_in_flight() {
        let r = hold_reason(
            PRED,
            &Predecessor::Found(pred("cancelled", json!({}), review("ready"))),
        );
        assert!(
            r.contains("ABANDONED") && r.contains("cancelled, no landing recorded"),
            "{r}"
        );
    }

    /// (4) NO SUCH JOB — a human must fix the REFERENCE, which is a
    /// different repair from breaking a live edge, so it gets different
    /// words. The line also says this should have been impossible, so the
    /// reader knows to suspect the write path and not the car.
    #[test]
    fn a_dangling_edge_says_the_job_does_not_exist() {
        let r = hold_reason(PRED, &Predecessor::Absent);
        assert!(r.contains("DOES NOT EXIST"), "{r}");
        assert!(
            r.contains("a human must fix metadata.boards_after"),
            "fix the reference, do not break the edge: {r}"
        );
        assert!(r.contains("ref-checked"), "say why this is surprising: {r}");
    }

    /// THE ASSERTION THE FEATURE IS TRUSTED ON: no two of the four read
    /// the same, and each side of the needs-a-human line is recognisable
    /// without reading the whole sentence.
    #[test]
    fn the_four_situations_are_told_apart_by_their_words() {
        let in_flight = hold_reason(
            PRED,
            &Predecessor::Found(pred("open", json!({}), review("ready"))),
        );
        let abandoned = hold_reason(
            PRED,
            &Predecessor::Found(pred("closed", json!({"outcome": "abandoned"}), json!([]))),
        );
        let absent = hold_reason(PRED, &Predecessor::Absent);
        let unjudged = match boards_after_outcome(PRED, &Predecessor::Unreadable("boom".into())) {
            EdgeOutcome::BoardUnjudged(note) => note,
            other => panic!("an unreadable edge must still board: {other:?}"),
        };
        let landed = boards_after_outcome(
            PRED,
            &Predecessor::Found(pred("closed", json!({"outcome": "merged"}), json!([]))),
        );

        let all = [&in_flight, &abandoned, &absent, &unjudged];
        for (i, a) in all.iter().enumerate() {
            for b in all.iter().skip(i + 1) {
                assert_ne!(a, b, "two situations read identically");
            }
        }
        assert_eq!(landed, EdgeOutcome::Board, "landed is not a refusal at all");
        // The one-glance test: does this need a person?
        assert!(!in_flight.contains("human") && in_flight.contains("no action needed"));
        assert!(abandoned.contains("a human must") && !abandoned.contains("no action needed"));
        assert!(absent.contains("a human must") && !absent.contains("no action needed"));
        assert!(unjudged.contains("boarding anyway"));
    }

    /// THE HAZARD THIS CAR WAS WARNED ABOUT. A read failure must not
    /// hold the dock: the conductor boards the car it cannot judge and
    /// says why, loudly. Refusing everything it could not evaluate would
    /// freeze every landing, and the gate does not run the conductor.
    #[test]
    fn an_unreadable_edge_boards_the_car_and_says_why() {
        let note = match boards_after_outcome(
            PRED,
            &Predecessor::Unreadable("HTTP 503 Service Unavailable".into()),
        ) {
            EdgeOutcome::BoardUnjudged(n) => n,
            other => panic!("fail-open is the whole point: {other:?}"),
        };
        assert!(note.contains("HTTP 503"), "carry the cause: {note}");
        assert!(note.contains("boarding anyway"), "{note}");
        assert!(
            note.contains("would stop every train"),
            "say why fail-open is the right choice here: {note}"
        );
    }

    /// THE REGRESSION THAT MATTERS MOST: every car in flight today has
    /// no edge, and must behave exactly as it did before this car.
    #[test]
    fn a_car_with_no_edge_declares_no_predecessor() {
        for md in [
            json!({"branch": "fix/x"}),
            json!({"branch": "fix/x", "boards_after": ""}),
            json!({"branch": "fix/x", "boards_after": "   "}),
            json!({"branch": "fix/x", "boards_after": Value::Null}),
        ] {
            let car = json!({"id": "c", "status": "open", "metadata": md});
            assert_eq!(
                declared_predecessor(&car),
                None,
                "no edge, or a cleared one, is not a constraint: {car}"
            );
        }
        let declared = json!({"id": "c", "metadata": {"boards_after": PRED}});
        assert_eq!(declared_predecessor(&declared).as_deref(), Some(PRED));
        assert_eq!(
            boards_after_of(&json!({"boards_after": PRED})).as_deref(),
            Some(PRED),
            "the dock reads the edge off a Job's metadata, the conductor off the packet — \
             one reading of one key"
        );
    }

    /// THE DOCK'S HALF (backlog 4142d821): a hold names what it waits
    /// behind as DATA, so the dock region can say "waiting behind X"
    /// without parsing the reason sentence back apart.
    #[test]
    fn a_hold_names_what_it_waits_behind() {
        let in_flight = Predecessor::Found(pred("open", json!({}), review("ready")));
        match boards_after_outcome(PRED, &in_flight) {
            EdgeOutcome::Hold(h) => {
                assert_eq!(h.behind, "fix/the-predecessor (car bbbbbbbb)");
                assert!(h.reason.contains(&h.behind), "{}", h.reason);
            }
            other => panic!("{other:?}"),
        }
        match boards_after_outcome(PRED, &Predecessor::Absent) {
            EdgeOutcome::Hold(h) => assert_eq!(h.behind, "car bbbbbbbb", "no packet, no branch"),
            other => panic!("{other:?}"),
        }
    }
}
