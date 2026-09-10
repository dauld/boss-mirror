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
//! NOT THE BOARD. This is the fact a board needs, and nothing more
//! (the item's own scope line: *"Not the board itself."*).

use anyhow::{Result, bail};
use boss_jobs::car::{self, BUILD, BUILD_SLUG};
use serde_json::Value;

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
    backlog_item: Option<&str>,
    actor: &str,
    host: &str,
    worktree: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> Value {
    let mut body = car::car_body(branch, summary, backlog_item, None);
    if let Some(md) = body.get_mut("metadata").and_then(Value::as_object_mut) {
        md.extend(car::build_start(actor, host, worktree, now));
    }
    body
}

/// `boss car open <branch>` — file the car at the START of the build.
pub(crate) async fn open(
    branch: &str,
    summary: &str,
    excludes: &str,
    backlog_item: Option<String>,
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
    let backlog_item = match backlog_item {
        None => None,
        Some(given) => {
            let mut all = Vec::new();
            for kind in ["backlog-item", "user-feedback"] {
                all.extend(crate::gate::rows(
                    crate::gate::api(
                        &http,
                        reqwest::Method::GET,
                        &format!("/api/jobs?kind={kind}&limit=200"),
                        None,
                    )
                    .await?,
                ));
            }
            let full = crate::park::resolve_job_id(&all, &given)?;
            if full != given {
                println!("boss car: backlog-item {given} -> {full}");
            }
            Some(full)
        }
    };

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
            branch,
            summary,
            backlog_item.as_deref(),
            &actor,
            &host,
            &worktree,
            now,
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
    if let Some(item) = backlog_item.as_deref() {
        crate::park::route_linked_item(&http, item, &id, branch, "car").await;
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
            None,
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
            Some("be025b44-2725-4db5-90d9-f16aba3844c6"),
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
}
