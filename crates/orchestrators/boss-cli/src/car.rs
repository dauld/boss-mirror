//! `boss car open <branch>` — the car exists from the first minute of
//! the build, not from the moment it succeeds.
//!
//! WHY THIS EXISTS (backlog be025b44). Auto-park filed the
//! `ship-a-change` packet when the gate went GREEN, which is the END of
//! the build. Everything before that left no trace: an agent working for
//! forty minutes, a builder blocked, two agents on the same file. The
//! system of record learned about the work only once it had succeeded.
//!
//! MEASURED, TWICE. On 2026-09-08 three builder sessions died mid-flight
//! and nothing anywhere said so — the only symptom was a twin car
//! appearing on the dock later, filed by a retry loop that outlived its
//! agent. On 2026-09-09 four builders ran for 19, 26, 31 and 47 minutes
//! and the yard showed an empty dock throughout.
//!
//! SO THE BUILDER OPENS THE CAR. This verb files the packet at `opened`,
//! declares the `scope` it was given, CLAIMS the `build` step, and
//! records who is building, on which host, in which worktree, since
//! when. A green then FINISHES that packet — `boss park` and the
//! auto-park handler both adopt a car that is already building rather
//! than filing a second — so a branch has exactly ONE packet from its
//! first minute, which closes the twinning failure mode by construction:
//! there is no window in which "no car" is the right answer.
//!
//! AND IT STATES WHICH ITEM IT IS FOR (backlog 90d291bb). Exactly one of
//! `--backlog-item` (this car IS that item's build — the item closes when
//! the car lands), `--partial-item` (one piece of it — provenance only,
//! the item stays open) or `--no-item <REASON>`. The refusal when none is
//! given is `boss gate`'s own, shared rather than copied: see
//! [`ItemAnswer::check`].
//!
//! WHAT `--park-*` DOES NOW. It confirms. Saying the same thing again at
//! the gate is ACCEPTED SILENTLY — requiring a builder to repeat
//! themselves would be friction with no safety gain, and the adopt
//! rewrites the same value it finds. Saying something DIFFERENT still
//! goes through `supersede` in the auto-park handler rather than being
//! refused: a gate that refuses at green wastes the whole build, where
//! superseding costs one warn line, and the gate's answer is the later
//! one made with the finished diff. `supersede` therefore becomes dead
//! code in the common case — the right direction for a guard that exists
//! only because two verbs could state different things about one fact.
//!
//! NOT THE BOARD. This is the fact a board needs, and nothing more
//! (the item's own scope line: *"Not the board itself."*).

use anyhow::{Result, bail};
use boss_jobs::car::{self, BUILD, BUILD_SLUG};
use serde_json::Value;

/// WHICH ITEM THIS CAR IS FOR — exactly one of three answers, stated at
/// BUILD START.
///
/// WHY HERE AND NOT ONLY AT THE GATE (backlog 90d291bb). `boss gate
/// --park-*` already demands one of three answers, but this verb took
/// only `--backlog-item`: the CLOSING edge. A builder opening a car for
/// ONE PIECE of a multi-piece item therefore had to either name the
/// closing edge — which is wrong, because the arrival rule follows that
/// key and would close an item with work outstanding — or say nothing,
/// losing the provenance for the whole build, which is exactly the
/// window the middle third exists to render.
///
/// THE DEEPER REASON IS THAT TWO VERBS COULD CONTRADICT EACH OTHER. An
/// open saying "close item X" and a gate saying "do not close X" were
/// both accepted, an hour apart, with nothing rejecting the pair; the
/// landed defence is `supersede` in the auto-park handler, which resolves
/// the disagreement in the safe direction. The builder who wrote it named
/// the better fix: *not being able to disagree is stronger than a guard
/// that resolves a disagreement.* So the answer is stated ONCE, here, at
/// build start, and `--park-*` merely confirms it.
#[derive(Debug, Clone, Default)]
pub(crate) struct ItemAnswer {
    /// This car IS that item's build — the one key the arrival rule
    /// follows, so the item CLOSES when the car lands.
    pub(crate) backlog_item: Option<String>,
    /// This car is ONE PIECE of that item: recorded as provenance under
    /// a key no rule reads, so the item stays open for its other pieces.
    pub(crate) partial_item: Option<String>,
    /// This car answers no item, and which kind of item-less car it is.
    pub(crate) no_item: Option<String>,
}

impl ItemAnswer {
    /// REFUSED BY THE GATE'S OWN FUNCTION — one definition of which
    /// answers are legal and what the refusal says (CLAUDE.md §9a).
    ///
    /// `ParkIntent::require_item_answer` is purely SYNTACTIC: it reads
    /// which of the three were given and nothing else — never the branch,
    /// never the diff — which is what makes it safe to share between a
    /// verb that runs before the work and one that runs after it. A
    /// second copy here would be the same fact living twice, and the
    /// copies would drift in the direction that matters most: what the
    /// refusal tells the builder to type.
    ///
    /// THE SCOPE IS PASSED IN because it is what makes the intent
    /// non-empty. `require_item_answer` returns `Ok` for an EMPTY intent
    /// — no `--park-*` flag at all is a plain gate, which does not file a
    /// car and owes no answer. An open always carries its summary and
    /// excludes (both required), so an open's intent is never empty and
    /// the check always runs.
    ///
    /// ONLY THE FLAG PREFIX IS ADAPTED. The gate spells these
    /// `--park-backlog-item`; this verb spells them `--backlog-item`. The
    /// rewrite is one mechanical substitution, pinned by
    /// `an_open_with_no_item_answer_is_refused_naming_all_three`, so a
    /// reworded gate refusal that stopped naming its flags reds a test
    /// rather than printing flags that do not exist.
    fn check(&self, summary: &str, excludes: &str) -> Result<()> {
        crate::gate::ParkIntent {
            summary: Some(summary.to_string()),
            excludes: Some(excludes.to_string()),
            backlog_item: self.backlog_item.clone(),
            partial_item: self.partial_item.clone(),
            no_item: self.no_item.clone(),
            ..Default::default()
        }
        .require_item_answer()
        .map_err(|e| anyhow::anyhow!(in_this_verbs_spelling(&e.to_string())))
    }

    /// The provenance keys this answer writes on the car — the core
    /// builder, so the open records them in exactly the shape the gate's
    /// auto-park and `boss park` write (CLAUDE.md §9a). Blank values are
    /// omitted, never nulled.
    fn provenance(&self) -> serde_json::Map<String, Value> {
        car::item_provenance(self.partial_item.as_deref(), self.no_item.as_deref())
    }

    /// Both item ids resolved from a short prefix to the full id, BEFORE
    /// anything is filed — so a typo costs a line of output.
    ///
    /// `backlog_item` is a declared edge the API ref-checks at the POST;
    /// `partial_item` is NOT checked by anything, which is precisely why
    /// it is resolved here. An unresolvable id in the one field nothing
    /// validates would otherwise record provenance pointing at nothing,
    /// and the next reader would have no way to tell that from a real
    /// link (never write an id you did not read).
    async fn resolve_ids(self, http: &reqwest::Client) -> Result<Self> {
        if self.backlog_item.is_none() && self.partial_item.is_none() {
            return Ok(self);
        }
        // Both kinds a car can answer, concatenated so an id matching one
        // of each is reported ambiguous rather than resolved by whichever
        // query ran first (381a4872).
        let mut all = Vec::new();
        for kind in ["backlog-item", "user-feedback"] {
            all.extend(crate::gate::rows(
                crate::gate::api(
                    http,
                    reqwest::Method::GET,
                    &format!("/api/jobs?kind={kind}&limit=200"),
                    None,
                )
                .await?,
            ));
        }
        let resolve = |flag: &str, given: Option<String>| -> Result<Option<String>> {
            let Some(given) = given else {
                return Ok(None);
            };
            let full = crate::park::resolve_job_id(&all, &given)?;
            if full != given {
                println!("boss car: {flag} {given} -> {full}");
            }
            Ok(Some(full))
        };
        Ok(Self {
            backlog_item: resolve("--backlog-item", self.backlog_item)?,
            partial_item: resolve("--partial-item", self.partial_item)?,
            no_item: self.no_item,
        })
    }
}

/// The gate's refusal, in this verb's flag spelling: `--park-backlog-item`
/// is `--backlog-item` here. One mechanical substitution over a shared
/// message, rather than a second message to keep in step with it.
///
/// The one line of our own says WHEN the answer is being asked for, which
/// the shared text cannot: it was written for a verb that runs after the
/// build, and here the build has not started.
fn in_this_verbs_spelling(refusal: &str) -> String {
    format!(
        "a car states which item it is for when its build STARTS — the gate then only \
         has to confirm it.\n{}",
        refusal.replace("--park-", "--")
    )
}

/// What `boss car open` does about a branch, decided from the system of
/// record alone — so "does it ever file a second car" is a unit test and
/// not a live incident.
#[derive(Debug)]
enum OpenAction<'a> {
    /// The branch already has a live car. Name it; file nothing.
    Already(&'a Value),
    /// The branch already landed. Refuse, with the reason.
    Refuse(String),
    /// No car for this branch: open one.
    File,
}

/// PURE: the whole decision.
///
/// ONE QUESTION, ONE DEFINITION. "Is there a live car for this branch"
/// is `boss_jobs::car::open_car_for` — the same question `boss gate`'s
/// launch guard and the auto-park handler ask, keyed on
/// `metadata.branch` (never the Subject, which `boss rerail` leaves
/// behind) and extended by this car to count a BUILDING car as live.
/// Three measured twins came from three places answering it differently
/// (d052afad, 02165b1d, 6790175e); this verb adds a fourth caller, not a
/// fourth answer (CLAUDE.md §9a).
///
/// The landed test reads the `merged` marker off the OPEN cars — the
/// conductor stamps it before the dispatcher closes the Job — so it
/// costs no extra query. A branch whose car is already CLOSED as merged
/// is not caught here; `boss gate`'s landed guard, which fetches main
/// and the closed cars, still refuses it before any gate is spent.
fn open_action<'a>(cars: &'a [Value], branch: &str) -> OpenAction<'a> {
    if let Some(landed) = car::landed_car_for(cars, branch) {
        let id = landed.get("id").and_then(Value::as_str).unwrap_or("?");
        return OpenAction::Refuse(format!(
            "{branch} already landed on main — car {id} carried it. Opening a car for \
             merged content files a packet for work nobody will do, and at green the same \
             mistake cost a cancelled train and a car abandoned by hand (backlog \
             610537b2). Cut a new branch for the new change."
        ));
    }
    match car::open_car_for(cars, branch) {
        Some(c) => OpenAction::Already(c),
        None => OpenAction::File,
    }
}

/// A branch this verb will open a car for, or the refusal.
///
/// `train/*` is the conductor's namespace — a train branch is a consist,
/// not a change — and the dock already filters it out by prefix when it
/// counts parked cars. Opening a ship-a-change packet for one would put
/// a car where the boarding logic deliberately looks away.
fn check_branch(branch: &str) -> Result<&str> {
    let b = branch.trim();
    if b.is_empty() {
        bail!("name the branch whose build is starting");
    }
    if b.starts_with("train/") {
        bail!(
            "{b} is a train branch, not a change. `train/*` is the conductor's namespace \
             — a consist, not a build — and the dock skips it by prefix when it counts \
             parked cars, so a car filed for one would be invisible to boarding."
        );
    }
    Ok(b)
}

/// PURE: the packet body an open files — the shared car body, plus the
/// build-start facts.
///
/// `delivery_channel` is deliberately absent: it is classified from the
/// diff, and at build start there is no diff yet. The gate stamps it, and
/// the finish (`regate_patch` / the adopt path) carries it onto the car
/// then — which is also how a re-gated car gets a fresh one.
fn open_body(
    branch: &str,
    summary: &str,
    item: &ItemAnswer,
    actor: &str,
    host: &str,
    worktree: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> Value {
    let mut body = car::car_body(branch, summary, item.backlog_item.as_deref(), None);
    if let Some(md) = body.get_mut("metadata").and_then(Value::as_object_mut) {
        md.extend(car::build_start(actor, host, worktree, now));
        md.extend(item.provenance());
    }
    body
}

/// `boss car open <branch>` — file the car at the START of the build.
pub(crate) async fn open(
    branch: &str,
    summary: &str,
    excludes: &str,
    item: ItemAnswer,
    dry: bool,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<()> {
    let branch = check_branch(branch)?;
    if summary.trim().is_empty() || excludes.trim().is_empty() {
        bail!(
            "--summary and --excludes are both required: the `scope` step asks what the \
             change contains and what it deliberately leaves out, and the registry row \
             says why it is asked BEFORE the work — that is the only moment the decision \
             keeps a PR small."
        );
    }
    // WHICH ITEM — before the actor, the reads and the POST, so a missing
    // answer costs a line of output rather than a half-filled packet. The
    // refusal is the GATE'S OWN (see `ItemAnswer::check`).
    item.check(summary, excludes)?;
    let http = reqwest::Client::new();

    // The actor FIRST, before anything is created: a write nobody names
    // is refused, and the refusal names both fixes (backlog 5083d6f5).
    // Resolved here so the refusal costs a line of output rather than a
    // half-filled packet.
    let actor = crate::identity::sign(&reqwest::Method::POST, "/api/jobs")?;

    // Resolve a short backlog id BEFORE filing, so a bad reference costs
    // a line rather than a rejected POST. Both kinds a car can answer,
    // concatenated so an id matching one of each is reported ambiguous
    // rather than resolved by whichever query ran first (381a4872).
    let item = item.resolve_ids(&http).await?;

    // EVERY OPEN CAR, PAGED. A limit is not a filter, and the question
    // "does this branch already have a car" must not answer `None`
    // because the car sat past page one — that false negative is what
    // filed three twins (a-limit-is-not-a-filter; 832 ship-a-change
    // packets existed on 2026-09-09).
    let cars = crate::gate::all_open_cars(&http).await?;
    match open_action(&cars, branch) {
        OpenAction::Refuse(why) => bail!("boss car: REFUSED — {why}"),
        OpenAction::Already(existing) => {
            let id = existing.get("id").and_then(Value::as_str).unwrap_or("?");
            let state = if car::is_boarded(existing) {
                "aboard a train"
            } else if car::is_parked(existing) {
                "parked at the dock"
            } else {
                "already building"
            };
            let by = existing
                .pointer("/metadata/built_by")
                .and_then(Value::as_str)
                .unwrap_or("someone unrecorded");
            let since = existing
                .pointer("/metadata/build_started_at")
                .and_then(Value::as_str)
                .unwrap_or("an unrecorded time");
            println!(
                "boss car: car {} is {state} for {branch} — no second car filed.\n  \
                 opened by {by} at {since}.\n  \
                 A branch has exactly ONE packet; if that build is yours, carry on and \
                 gate it. If it is not, you are about to work on a branch someone else \
                 holds.",
                &id[..8.min(id.len())]
            );
            return Ok(());
        }
        OpenAction::File => {}
    }

    let host = crate::prove::host();
    let worktree = std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_default();

    if dry {
        println!(
            "boss car: DRY would open a car for {branch} as {actor} on {host} \
             ({worktree}), scope declared and build claimed"
        );
        return Ok(());
    }

    let created = crate::gate::api(
        &http,
        reqwest::Method::POST,
        "/api/jobs",
        Some(open_body(
            branch, summary, &item, &actor, &host, &worktree, now,
        )),
    )
    .await?;
    let id = created
        .as_ref()
        .and_then(|c| c.get("data").unwrap_or(c).get("id"))
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("the create returned no id — refusing to call that a car"))?
        .to_string();

    // A 201 IS A CLAIM; THE READ-BACK IS THE FACT — and the read is
    // needed anyway, because the step ids only exist once the packet
    // does (`boss job file` encodes the same rule).
    let car_json = crate::gate::api(
        &http,
        reqwest::Method::GET,
        &format!("/api/jobs/{id}"),
        None,
    )
    .await?
    .ok_or_else(|| anyhow::anyhow!("filed car {id} and the API will not read it back"))?;

    // DECLARE THE SCOPE. The writes are decided in core, shared with the
    // two finishers, and skip anything already done.
    for w in car::open_writes(&car_json, summary, excludes, now).map_err(anyhow::Error::msg)? {
        crate::gate::api(
            &http,
            reqwest::Method::PUT,
            &format!("/api/jobs/{id}/steps/{}", w.step_id),
            Some(w.body),
        )
        .await?;
    }

    // CLAIM THE BUILD. Through the claim door, not a status PUT: it is a
    // Ready→Active compare-and-set that records the claimant and answers
    // 409 with the holder, so "two agents on the same branch" is a
    // refusal naming the other one instead of a second car.
    let build_id = car::step_id_for(&car_json, BUILD_SLUG, BUILD)
        .ok_or_else(|| anyhow::anyhow!("car {id} has no `{BUILD_SLUG}` step to claim"))?;
    let claimed = crate::gate::api(
        &http,
        reqwest::Method::POST,
        &format!("/api/jobs/{id}/steps/{build_id}/claim"),
        None,
    )
    .await;
    match claimed {
        Ok(_) => {}
        Err(e) => println!(
            "boss car: car {} is open but its `build` step would not claim ({e}) — \
             the packet exists and records the build; claim it from the step surface, \
             or carry on and gate, which completes it either way.",
            &id[..8.min(id.len())]
        ),
    }

    // CONFIRM BY READING IT BACK, not by the status codes above: a step
    // PUT carrying an unknown field is a silent 204 no-op, and a claim
    // that lost its race is a 409 we chose not to fail on.
    let after = crate::gate::api(
        &http,
        reqwest::Method::GET,
        &format!("/api/jobs/{id}"),
        None,
    )
    .await?
    .ok_or_else(|| anyhow::anyhow!("could not read car {id} back to confirm it"))?;
    let step_state = |slug: &str, title: &str| {
        car::find_step(&after, slug, title)
            .and_then(|s| s.get("status"))
            .and_then(Value::as_str)
            .unwrap_or("?")
            .to_string()
    };
    println!(
        "boss car: car {} open for {branch} — scope {}, build {} — confirmed by reading \
         it back",
        &id[..8.min(id.len())],
        step_state(car::SCOPE_SLUG, car::SCOPE),
        step_state(BUILD_SLUG, BUILD),
    );
    println!(
        "  built by {actor} on {host} ({worktree}) — the green will FINISH this packet, \
         not file a second."
    );

    // THE CAR IS THE ITEM'S BUILD — and saying so at OPEN rather than at
    // green is the point: the item's `build` step opens when the build
    // starts. Last, so a failure here costs the routing and not the car.
    if let Some(closes) = item.backlog_item.as_deref() {
        crate::park::route_linked_item(&http, closes, &id, branch, "car").await;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const BRANCH: &str = "feat/a-car-opens-when-the-build-starts";

    fn at(s: &str) -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339(s).unwrap().into()
    }

    /// The three answers, each on its own — the shape the CLI hands in.
    fn closes(id: &str) -> ItemAnswer {
        ItemAnswer {
            backlog_item: Some(id.into()),
            ..Default::default()
        }
    }
    fn partial(id: &str) -> ItemAnswer {
        ItemAnswer {
            partial_item: Some(id.into()),
            ..Default::default()
        }
    }
    fn no_item(reason: &str) -> ItemAnswer {
        ItemAnswer {
            no_item: Some(reason.into()),
            ..Default::default()
        }
    }
    /// The scope every open carries — what makes an open's intent never
    /// empty, which is why the gate's refusal runs on it.
    const SUMMARY: &str = "A car states its item at build start";
    const EXCLUDES: &str = "Not the gate's own refusal, which is reused";
    fn check(a: &ItemAnswer) -> Result<()> {
        a.check(SUMMARY, EXCLUDES)
    }

    /// A car as this verb leaves it: scope declared, build claimed, gate
    /// and review still ahead.
    fn building(id: &str, branch: &str) -> Value {
        json!({
            "id": id,
            "kind": "ship-a-change",
            "status": "open",
            "metadata": {"branch": branch},
            "steps": [
                {"id": "s-scope", "spec_slug": "scope", "title": car::SCOPE,
                 "status": "completed"},
                {"id": "s-build", "spec_slug": "build", "title": BUILD, "status": "active"},
                {"id": "s-gate", "spec_slug": "gate", "title": car::GATE, "status": "pending"},
                {"id": "s-review", "spec_slug": "review", "title": car::REVIEW,
                 "status": "pending"},
            ],
        })
    }

    /// A car at the dock: gated, waiting for a train.
    fn parked(id: &str, branch: &str) -> Value {
        let mut c = building(id, branch);
        c["steps"][2]["status"] = json!("completed");
        c["steps"][3]["status"] = json!("ready");
        c
    }

    /// EXACTLY ONE PACKET PER BRANCH, FROM THE FIRST MINUTE. A second
    /// `boss car open` on a branch that already has a live car names it
    /// and files nothing — the whole argument for this car is that a
    /// branch never has two.
    #[test]
    fn a_branch_that_already_has_a_live_car_gets_no_second_one() {
        for existing in [building("b1", BRANCH), parked("p1", BRANCH)] {
            let id = existing["id"].as_str().unwrap().to_string();
            match open_action(std::slice::from_ref(&existing), BRANCH) {
                OpenAction::Already(c) => assert_eq!(c["id"], id.as_str()),
                other => panic!("{id} is live; expected Already, got {other:?}"),
            }
        }
        // Aboard a train is live too: the build is over, and re-opening
        // would put a second packet on a branch in transit (02165b1d).
        let mut aboard = parked("a1", BRANCH);
        aboard["metadata"]["train"] = json!("train-9");
        match open_action(&[aboard], BRANCH) {
            OpenAction::Already(c) => assert_eq!(c["id"], "a1"),
            other => panic!("expected Already, got {other:?}"),
        }
    }

    /// A branch with no car files one — and another branch's car is not
    /// this branch's.
    #[test]
    fn a_branch_with_no_car_opens_one() {
        assert!(matches!(open_action(&[], BRANCH), OpenAction::File));
        assert!(matches!(
            open_action(&[building("b1", "feat/other")], BRANCH),
            OpenAction::File
        ));
        // Spent history is not a live car: a car past review, with no
        // train, has had its life.
        let mut spent = parked("s1", BRANCH);
        spent["steps"][3]["status"] = json!("completed");
        assert!(matches!(open_action(&[spent], BRANCH), OpenAction::File));
    }

    /// A BRANCH THAT ALREADY LANDED IS NOT A BUILD. Opening a car for
    /// content already on main files a packet for work nobody will do,
    /// and the same mistake at green cost a cancelled train and a car
    /// abandoned by hand (backlog 610537b2).
    #[test]
    fn a_landed_branch_is_refused() {
        let mut landed = parked("l1", BRANCH);
        landed["metadata"]["merged"] = json!("true");
        landed["metadata"]["merge_ref"] = json!("b641f3adcf47");
        match open_action(&[landed], BRANCH) {
            OpenAction::Refuse(why) => {
                assert!(why.contains("already"), "{why}");
                assert!(why.contains("l1"), "the refusal names the car: {why}");
            }
            other => panic!("expected Refuse, got {other:?}"),
        }
    }

    /// THE PACKET SAYS WHO, WHERE AND SINCE WHEN. That is the fact the
    /// middle third renders; without it a board has an absence to draw.
    #[test]
    fn the_opened_packet_records_the_actor_the_host_and_the_worktree() {
        let body = open_body(
            BRANCH,
            "A car opens when the build starts. And more.",
            &no_item("asked for in conversation"),
            "claude@algedonic.dev",
            "boss-dev-0",
            "/work/boss/.claude/worktrees/agent-a23",
            at("2026-09-10T18:00:00Z"),
        );
        // The shared car body, unchanged: one definition of what a
        // ship-a-change packet is (CLAUDE.md §9a).
        assert_eq!(body["kind"], "ship-a-change");
        assert_eq!(body["metadata"]["branch"], BRANCH);
        assert_eq!(body["subject"]["id"], BRANCH);
        assert_eq!(
            body["title"], "A car opens when the build starts",
            "the title is the summary's first sentence, as it is at park"
        );
        // ...plus the build-start facts.
        assert_eq!(body["metadata"]["built_by"], "claude@algedonic.dev");
        assert_eq!(body["metadata"]["build_host"], "boss-dev-0");
        assert_eq!(
            body["metadata"]["build_worktree"],
            "/work/boss/.claude/worktrees/agent-a23"
        );
        assert_eq!(body["metadata"]["build_started_at"], "2026-09-10T18:00:00Z");
        assert!(body["metadata"].get("backlog_item").is_none());
    }

    /// The backlog edge rides from the first minute, not from green — so
    /// a reader of the item can see its build is under way.
    #[test]
    fn the_backlog_edge_is_carried_from_the_first_minute() {
        let body = open_body(
            BRANCH,
            "s",
            &closes("be025b44-2725-4db5-90d9-f16aba3844c6"),
            "claude@algedonic.dev",
            "",
            "",
            at("2026-09-10T18:00:00Z"),
        );
        assert_eq!(
            body["metadata"]["backlog_item"],
            "be025b44-2725-4db5-90d9-f16aba3844c6"
        );
        // Unknown host/worktree are OMITTED, never nulled: the metadata
        // door deletes a null key.
        assert!(body["metadata"].get("build_host").is_none());
        assert!(body["metadata"].get("build_worktree").is_none());
    }

    /// A `train/` branch is the conductor's, not a build's. Opening a
    /// car for one would put a ship-a-change packet on a train branch,
    /// which the dock already has to filter out by prefix.
    #[test]
    fn a_train_branch_is_not_a_car() {
        let e = check_branch("train/2026-09-10-1800")
            .unwrap_err()
            .to_string();
        assert!(e.contains("train/"), "{e}");
        let e = check_branch("  ").unwrap_err().to_string();
        assert!(e.contains("branch"), "{e}");
        assert!(check_branch(BRANCH).is_ok());
    }

    /// A CAR WITH NO ITEM ANSWER IS REFUSED, AND THE REFUSAL NAMES ALL
    /// THREE — in THIS verb's flag spelling.
    ///
    /// The refusal itself is `boss gate`'s: one definition of which
    /// answers are legal and what the refusal says (CLAUDE.md §9a). What
    /// is adapted here is only the flag prefix, and this test is the pin
    /// on that adaptation — a reworded gate refusal that stopped naming
    /// the flags would red here rather than silently printing the wrong
    /// ones.
    #[test]
    fn an_open_with_no_item_answer_is_refused_naming_all_three() {
        let e = check(&ItemAnswer::default()).unwrap_err().to_string();
        for flag in ["--backlog-item", "--partial-item", "--no-item"] {
            assert!(e.contains(flag), "the refusal names {flag}: {e}");
        }
        assert!(
            !e.contains("--park-"),
            "this verb's flags have no `park` in them — the gate's spelling must not leak \
             into a refusal printed by `boss car open`: {e}"
        );
    }

    /// EACH ANSWER, ALONE, IS ACCEPTED — and lands on the packet under
    /// the key its semantics demand. The closing edge is the ONLY key the
    /// arrival rule follows, so the other two must never write it.
    #[test]
    fn each_of_the_three_answers_is_recorded_under_its_own_key() {
        let body = |a: &ItemAnswer| {
            check(a).expect("one answer is enough");
            open_body(
                BRANCH,
                SUMMARY,
                a,
                "claude@algedonic.dev",
                "boss-dev-0",
                "/w",
                at("2026-09-11T09:00:00Z"),
            )
        };

        let closing = body(&closes("90d291bb-9772-4ccb-b520-6d934f0860b8"));
        assert_eq!(
            closing["metadata"]["backlog_item"],
            "90d291bb-9772-4ccb-b520-6d934f0860b8"
        );
        for k in [car::PARTIAL_ITEM, car::NO_ITEM_REASON] {
            assert!(closing["metadata"].get(k).is_none(), "{closing}");
        }

        let piece = body(&partial("cf0f5e2d-0000-0000-0000-000000000000"));
        assert_eq!(
            piece["metadata"][car::PARTIAL_ITEM],
            "cf0f5e2d-0000-0000-0000-000000000000"
        );
        assert!(
            piece["metadata"].get("backlog_item").is_none(),
            "ONE PIECE must never write the key that closes the item: {piece}"
        );
        assert!(piece["metadata"].get(car::NO_ITEM_REASON).is_none());

        let none = body(&no_item("David asked for this in conversation"));
        assert_eq!(
            none["metadata"][car::NO_ITEM_REASON],
            "David asked for this in conversation"
        );
        assert!(none["metadata"].get("backlog_item").is_none());
        assert!(none["metadata"].get(car::PARTIAL_ITEM).is_none());
    }

    /// TWO ANSWERS AT ONCE ARE REFUSED RATHER THAN RANKED. Which one is
    /// given decides whether the item CLOSES when the car lands, so
    /// there is nothing to rank.
    #[test]
    fn two_answers_at_once_are_refused_rather_than_ranked() {
        let id = "cf0f5e2d-0000-0000-0000-000000000000";
        let pairs = [
            (Some(id), Some(id), None),
            (Some(id), None, Some("asked in chat")),
            (None, Some(id), Some("asked in chat")),
            (Some(id), Some(id), Some("asked in chat")),
        ];
        for (b, p, n) in pairs {
            let a = ItemAnswer {
                backlog_item: b.map(str::to_string),
                partial_item: p.map(str::to_string),
                no_item: n.map(str::to_string),
            };
            let e = check(&a).unwrap_err().to_string();
            assert!(
                e.contains("exactly one"),
                "{b:?}/{p:?}/{n:?} must be refused, not ranked: {e}"
            );
            assert!(!e.contains("--park-"), "{e}");
        }
    }

    /// A BLANK ANSWER IS NOT AN ANSWER. A blank id records nothing and
    /// passes the ref check as "no claim to check"; a blank reason is the
    /// silent omission again with a flag in front of it.
    #[test]
    fn a_blank_id_or_a_blank_reason_is_refused() {
        for a in [closes("   "), partial("  "), no_item("   "), no_item("")] {
            let e = check(&a).unwrap_err().to_string();
            assert!(
                e.contains("--no-item"),
                "the refusal points at the escape: {e}"
            );
            assert!(!e.contains("--park-"), "{e}");
        }
    }
}
