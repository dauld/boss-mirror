//! Where the IT department's work comes from — the INPUT-channel mix.
//!
//! Design: "IT delivery channels", approved 2026-09-06 as a design-doc
//! packet; its file under docs/design/ was folded and deleted with the
//! 2026-09-10 docs cleanup (f5da586c), so the packet is the record.
//! A job has two orthogonal channels: `input_channel` (the lane it
//! entered through) and `delivery_channel` (how it ships). This module
//! answers the first, and reports the *mix* — the algedonic reading
//! David watches: work driven by USER FEEDBACK is proactive (building
//! what is wanted), work driven by MONITORING and ERROR DISCOVERY is
//! reactive (firefighting). A department whose input tilts toward
//! firefighting is in pain; the distribution over time is the signal.
//!
//! Read-only. Classification is a pure function of the job, heuristic
//! today and refined as each lane earns its own admission protocol
//! (only user-feedback is a real lane so far); an ambiguous job is
//! named `unclassified` rather than guessed into false precision.

use std::collections::BTreeMap;

use anyhow::Result;
use serde_json::{Value, json};

/// The lane vocabulary itself now lives in `boss_jobs::channels`, so
/// the CLI door and the machine filers in `boss-dispatcher-handlers`
/// read ONE definition of the key and the labels (backlog b2d9b432,
/// CLAUDE.md §9a). Re-exported under the local names so this module's
/// callers and tests read unchanged. What stays here is what is
/// genuinely the CLI's: the pre-field inference, the provenance split,
/// and the mix report.
pub(crate) use boss_jobs::channels::{FILEABLE_LANES, InputChannel, RECORDED_KEY};

/// When `metadata.input_channel` started being written (c5dc81a1, the
/// car that added the field). Packets opened before it predate the
/// field: they keep the keyword inference, MARKED as inference, the way
/// `agent_runs` marks its pre-instrumentation era rather than
/// back-filling a guess into rows nobody measured. A packet opened after
/// it with no lane means the filer did not say — which is actionable.
/// Midnight AFTER the car lands, so the boundary never accuses a filer
/// who used a door that was not yet live.
const FIELD_LIVE_AT: &str = "2026-09-21T00:00:00Z";

/// How a job's lane was arrived at — the distinction is the whole point
/// of c5dc81a1. A RECORDED lane is a fact its filer wrote; an INFERRED
/// one is keyword overlap, kept only for the pre-field era; NOT-SAID is
/// a filer who skipped the field. Reporting the three together is what
/// stops a share computed over the minority from reading as a measure of
/// the whole (367b2dbe).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Provenance {
    Recorded,
    Inferred,
    NotSaid,
}

/// The three counts behind any reading of the mix.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Provenances {
    pub(crate) recorded: usize,
    /// Pre-field rows: no `input_channel`, lane guessed from words
    /// (possibly to `unclassified`, when the words matched nothing).
    pub(crate) inferred: usize,
    pub(crate) not_stated: usize,
}

fn field<'a>(job: &'a Value, key: &str) -> &'a str {
    job.get("metadata")
        .and_then(|m| m.get(key))
        .and_then(Value::as_str)
        .unwrap_or("")
}

/// Was this packet filed before `metadata.input_channel` existed? A
/// packet with no readable `opened_at` is treated as pre-field, because
/// the conservative error is to keep inferring, never to report a filer
/// as silent when we cannot date them.
fn pre_field(job: &Value) -> bool {
    let opened = match field(job, "opened_at") {
        "" => job.get("opened_at").and_then(Value::as_str).unwrap_or(""),
        s => s,
    };
    match chrono::DateTime::parse_from_rfc3339(opened) {
        Ok(t) => match chrono::DateTime::parse_from_rfc3339(FIELD_LIVE_AT) {
            Ok(live) => t < live,
            Err(_) => true,
        },
        Err(_) => true,
    }
}

/// Classify a work-originating job by its input lane AND say how that
/// lane was arrived at. Pure: same job, same answer.
///
/// The order is the fix (c5dc81a1). A lane the filer RECORDED wins over
/// every keyword, because the filer knows the origin and a heuristic
/// never will: the words in a title like "deny_unknown_fields is inert
/// under serde flatten" are unclassifiable by construction while its
/// origin — a builder found it mid-car — is perfectly well known.
/// Measured over 310 backlog-items on 2026-09-20, the keywords matched
/// 58 and missed 252, and the miss rate CLIMBED (59% on 09-17, 83% on
/// 09-18, 91% on 09-19) as more of the work came from one session whose
/// vocabulary the classifier never held.
pub(crate) fn classify(job: &Value) -> (InputChannel, Provenance) {
    if let Some(lane) = InputChannel::parse(field(job, RECORDED_KEY)) {
        return (lane, Provenance::Recorded);
    }
    // A kind that names its own lane is a recorded fact too — structured
    // data, not vocabulary overlap — so it needs no field.
    let kind = job.get("kind").and_then(Value::as_str).unwrap_or("");
    if kind == "user-feedback" {
        return (InputChannel::UserFeedback, Provenance::Recorded);
    }
    if kind.starts_with("maintenance-") || kind == "rotate-a-credential" {
        return (InputChannel::Scheduled, Provenance::Recorded);
    }
    if pre_field(job) {
        return (infer(job), Provenance::Inferred);
    }
    (InputChannel::Unclassified, Provenance::NotSaid)
}

/// The lane alone, for callers that only count. See [`classify`].
pub(crate) fn input_channel(job: &Value) -> InputChannel {
    classify(job).0
}

/// The pre-field guess: keyword overlap over reporter/source/area/title.
/// Kept ONLY as the fallback for rows filed before the field existed —
/// not extended. Better keywords were never the fix.
fn infer(job: &Value) -> InputChannel {
    let title = job
        .get("title")
        .and_then(Value::as_str)
        .or_else(|| {
            job.get("metadata")
                .and_then(|m| m.get("title"))
                .and_then(Value::as_str)
        })
        .unwrap_or("");
    let hay = format!(
        "{} {} {} {}",
        field(job, "reporter"),
        field(job, "source"),
        field(job, "area"),
        title
    )
    .to_lowercase();
    let has = |needle: &str| hay.contains(needle);

    if has("post-mortem") || has("5-whys") || has("5 whys") || has("boot-brick") || has("outage") {
        return InputChannel::PostMortem;
    }
    if has("cve") || has("advisor") || has("dependency") || has("toolchain") || has("upgrade") {
        return InputChannel::Dependency;
    }
    if has("review finding") || has("reviewer") || has("ultrareview") {
        return InputChannel::Review;
    }
    if has("red train") || has("red gate") || has("reddens") || has("gate-blind") || has("ci job") {
        return InputChannel::PipelineFailure;
    }
    if has("estate")
        || has("monitor")
        || has("telemetry")
        || has("alarm")
        || has("observ")
        || has("disk")
    {
        return InputChannel::Telemetry;
    }
    if has("design decision") || has("design-resolution") || has("open question") {
        return InputChannel::DesignResolution;
    }

    let reporter = field(job, "reporter").to_lowercase();
    let source = field(job, "source").to_lowercase();
    if reporter.contains("david") || source.contains("david") {
        return InputChannel::Roadmap;
    }
    if reporter.contains("claude") {
        return InputChannel::Discovery;
    }
    InputChannel::Unclassified
}

/// Count jobs per input lane. The mix, not the individual cars.
pub(crate) fn input_mix(jobs: &[Value]) -> BTreeMap<InputChannel, usize> {
    let mut mix: BTreeMap<InputChannel, usize> = BTreeMap::new();
    for job in jobs {
        *mix.entry(input_channel(job)).or_insert(0) += 1;
    }
    mix
}

/// Count jobs per lane over RECORDED rows only — the denominator any
/// health reading is entitled to. See [`proactive_share`].
pub(crate) fn recorded_mix(jobs: &[Value]) -> BTreeMap<InputChannel, usize> {
    let mut mix: BTreeMap<InputChannel, usize> = BTreeMap::new();
    for job in jobs {
        let (lane, how) = classify(job);
        if how == Provenance::Recorded {
            *mix.entry(lane).or_insert(0) += 1;
        }
    }
    mix
}

/// How the lanes were arrived at, across a set of jobs.
pub(crate) fn provenances(jobs: &[Value]) -> Provenances {
    let mut p = Provenances::default();
    for job in jobs {
        match classify(job).1 {
            Provenance::Recorded => p.recorded += 1,
            Provenance::Inferred => p.inferred += 1,
            Provenance::NotSaid => p.not_stated += 1,
        }
    }
    p
}

/// The proactive share: fraction of classified work coming from lanes
/// that build what is wanted. `None` when there is nothing to divide by,
/// so a caller never reports a health reading it cannot support.
pub(crate) fn proactive_share(mix: &BTreeMap<InputChannel, usize>) -> Option<f64> {
    let total: usize = mix.values().sum();
    if total == 0 {
        return None;
    }
    let proactive: usize = mix
        .iter()
        .filter(|(ch, _)| ch.is_proactive())
        .map(|(_, n)| *n)
        .sum();
    Some(proactive as f64 / total as f64)
}

/// `boss channels` — read recent work-originating jobs and report the
/// input-channel mix with the proactive-vs-reactive reading, the
/// delivery mix over the dock, and the per-tier mix (ba429e7f) over the
/// dock and over the trains landed since `since` (default: the last
/// [`TIER_WINDOW_DAYS`]).
pub async fn run(
    since: Option<chrono::NaiveDate>,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<()> {
    let http = reqwest::Client::new();

    // Backlog items are classified one by one, so they must all be
    // fetched — a `limit` is a page, not a filter, and a capped page is
    // a smaller question answered (a-limit-is-not-a-filter). Fetch wide
    // and say so if it still capped.
    let bl = crate::gate::api(
        &http,
        reqwest::Method::GET,
        "/api/jobs?kind=backlog-item&limit=1000",
        None,
    )
    .await?;
    let bl_total = bl
        .as_ref()
        .and_then(|b| b.get("total"))
        .and_then(Value::as_u64)
        .unwrap_or(0) as usize;
    let backlog: Vec<Value> = bl
        .as_ref()
        .and_then(|b| b.get("data"))
        .and_then(Value::as_array)
        .map(|r| r.to_vec())
        .unwrap_or_default();
    let capped = bl_total > backlog.len();

    // Every user-feedback job is the user-feedback lane, so it needs a
    // COUNT, not a full fetch — read `total` off a single-row page.
    let uf = crate::gate::api(
        &http,
        reqwest::Method::GET,
        "/api/jobs?kind=user-feedback&limit=1",
        None,
    )
    .await?;
    let uf_total = uf
        .as_ref()
        .and_then(|b| b.get("total"))
        .and_then(Value::as_u64)
        .unwrap_or(0) as usize;

    let mut mix = input_mix(&backlog);
    // The same rows read a second way: how each lane was ARRIVED at.
    // Printed beside the mix because a lane guessed from a title's words
    // is not the same kind of fact as one its filer recorded (c5dc81a1).
    let mut how = provenances(&backlog);
    let mut rmix = recorded_mix(&backlog);
    if uf_total > 0 {
        *mix.entry(InputChannel::UserFeedback).or_insert(0) += uf_total;
        // Counted without fetching the rows: the KIND is the lane, which
        // is a recorded fact whatever the packet's words say.
        *rmix.entry(InputChannel::UserFeedback).or_insert(0) += uf_total;
        how.recorded += uf_total;
    }
    let total: usize = mix.values().sum();
    println!("boss channels — where the work comes from (input mix)");
    println!(
        "  {} work-originating job(s) ({} backlog + {} feedback){}\n",
        total,
        backlog.len(),
        uf_total,
        if capped {
            " — backlog CAPPED, mix is a floor"
        } else {
            ""
        }
    );
    for (ch, n) in &mix {
        let pct = if total > 0 {
            (*n as f64) * 100.0 / (total as f64)
        } else {
            0.0
        };
        println!(
            "    {:<24} {:>4}  {:>5.1}%  {}",
            ch.label(),
            n,
            pct,
            if ch.is_proactive() {
                "proactive"
            } else {
                "reactive"
            }
        );
    }
    println!(
        "\n  how the lane is known: {} recorded · {} inferred from words (pre-field rows) \
         · {} filed without one",
        how.recorded, how.inferred, how.not_stated
    );
    // The headline is computed over RECORDED rows ONLY. It used to be
    // computed over every row and printed with no caveat, which made it
    // a confident number over the minority the keywords happened to
    // match — 58 of 310 on 2026-09-20 (367b2dbe, retired here). A
    // denominator that is mostly guesswork produces a reading nobody can
    // act on, so say what it covers or say n/a.
    let rtotal: usize = rmix.values().sum();
    match proactive_share(&rmix) {
        Some(s) => println!(
            "  proactive share: {:.0}% over the {} packet(s) whose lane is recorded  — {}",
            s * 100.0,
            rtotal,
            if s >= 0.5 {
                "building what is wanted"
            } else {
                "tilted toward firefighting"
            }
        ),
        None => println!(
            "  proactive share: n/a — no packet in the window carries a recorded lane, \
             and keyword overlap is not a reading"
        ),
    }

    // Delivery mix over the dock: how the work about to ship will ship.
    // A single fetch keeps the per-car diffs honest against the forge;
    // a car whose branch is gone (already merged) is skipped, not
    // miscounted.
    let _ = std::process::Command::new("git")
        .args(["fetch", "origin", "--quiet"])
        .status();
    // Every boardable car in the dock, not just page one: the dock
    // builds past a page (in-flight + parked + landed-but-unclosed
    // residue), and a bare `limit=` read undercounts the delivery mix
    // silently (a-limit-is-not-a-filter). `gate::all_open_cars` pages on
    // `total`.
    let cars = crate::gate::all_open_cars(&http).await?;
    let boardable = boardable_cars(cars);

    let mut dmix: std::collections::BTreeMap<DeliveryChannel, usize> =
        std::collections::BTreeMap::new();
    // The same diffs, read a second way: which tiers each car touches
    // (a car in several counts once in each) and its headline tier.
    let mut dock_tiers: BTreeMap<String, usize> = BTreeMap::new();
    let mut dock_headlines: BTreeMap<String, usize> = BTreeMap::new();
    let mut skipped = 0usize;
    for car in &boardable {
        let branch = car
            .get("metadata")
            .and_then(|m| m.get("branch"))
            .and_then(Value::as_str)
            .unwrap_or("");
        match (!branch.is_empty())
            .then(|| changed_paths_for(branch))
            .flatten()
        {
            Some(paths) => {
                *dmix.entry(delivery_channel(&paths)).or_insert(0) += 1;
                let tiers = software_tiers(&paths);
                for t in &tiers {
                    *dock_tiers.entry(t.clone()).or_insert(0) += 1;
                }
                *dock_headlines
                    .entry(software_tier(&tiers).unwrap_or_else(|| UNCLASSIFIED_TIER.to_string()))
                    .or_insert(0) += 1;
            }
            None => skipped += 1,
        }
    }
    let dtotal: usize = dmix.values().sum();
    println!(
        "\n  DELIVERY mix — {} car(s) in the dock{}",
        dtotal,
        if skipped > 0 {
            format!(" ({skipped} skipped: branch resolved by no forge ref)")
        } else {
            String::new()
        }
    );
    for (ch, n) in &dmix {
        let pct = if dtotal > 0 {
            (*n as f64) * 100.0 / (dtotal as f64)
        } else {
            0.0
        };
        println!("    {:<10} {:>4}  {:>5.1}%", ch.label(), n, pct);
    }
    if let Some(d) = dmix.get(&DeliveryChannel::Data) {
        let share = if dtotal > 0 {
            *d as f64 / dtotal as f64
        } else {
            0.0
        };
        println!(
            "\n  data-delivery share: {:.0}% — {}",
            share * 100.0,
            if share >= 0.5 {
                "shipping light"
            } else {
                "still mostly build-and-deploy"
            }
        );
    }

    // The per-tier mix (ba429e7f): over the dock, from the same diffs,
    // and over the trains landed in the window, from the stamp the
    // arrival wrote (or the backfill read off the merge commit).
    println!("\n  TIERS touched — {dtotal} car(s) in the dock (a car counts once per tier)");
    print_tier_mix(&dock_tiers, dtotal);
    println!("  headline tier (lowest rank wins) — {dtotal} car(s)");
    print_tier_mix(&dock_headlines, dtotal);

    let since =
        since.unwrap_or_else(|| (now - chrono::Duration::days(TIER_WINDOW_DAYS)).date_naive());
    print_landed(since, &landed_tier_reading(&http, since).await?);
    Ok(())
}

/// The default window for the landed-trains tier mix. Consolidation's
/// own cars touch core on purpose (the packet's data note), so the
/// reading is honest only over a window long enough to see them pass.
pub(crate) const TIER_WINDOW_DAYS: i64 = 30;

/// A car is in the dock when its "Open for review" step is ready — the
/// same predicate the conductor boards on.
fn is_open_for_review(car: &Value) -> bool {
    car.get("steps")
        .and_then(Value::as_array)
        .map(|steps| {
            steps.iter().any(|st| {
                st.get("title").and_then(Value::as_str) == Some("Open for review")
                    && st.get("status").and_then(Value::as_str) == Some("ready")
            })
        })
        .unwrap_or(false)
}

/// The boardable cars among the fully-gathered open cars — pure, so the
/// filter (and that it counts a car past page one, not just the first
/// page) is testable without a live API.
fn boardable_cars(cars: Vec<Value>) -> Vec<Value> {
    cars.into_iter().filter(is_open_for_review).collect()
}

/// How a change ships — the delivery channel, ordered lightest to
/// heaviest by reversibility/blast-radius. A car spanning several
/// artifact types ships on its HEAVIEST channel: a registry row plus a
/// crate still needs the build.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum DeliveryChannel {
    Data,
    Config,
    Software,
    Infra,
}

impl DeliveryChannel {
    pub(crate) fn label(self) -> &'static str {
        match self {
            DeliveryChannel::Data => "data",
            DeliveryChannel::Config => "config",
            DeliveryChannel::Software => "software",
            DeliveryChannel::Infra => "infra",
        }
    }

    /// The channel a stamped label names — the inverse of [`label`],
    /// for reading a car's `metadata.delivery_channel` back. `None` for
    /// a label this order does not know, so the caller chooses the
    /// default rather than this fn guessing one.
    ///
    /// [`label`]: DeliveryChannel::label
    pub(crate) fn parse(label: &str) -> Option<DeliveryChannel> {
        match label {
            "data" => Some(DeliveryChannel::Data),
            "config" => Some(DeliveryChannel::Config),
            "software" => Some(DeliveryChannel::Software),
            "infra" => Some(DeliveryChannel::Infra),
            _ => None,
        }
    }
}

/// The channel a TRAIN ships on: the heaviest of its cars', in the one
/// order a mixed car already resolves on (data < config < software <
/// infra — the enum's derived `Ord`). A car with no stamp, or a stamp
/// this order does not know, reads as software — the car's own default
/// (`yard.ts::deliveryChannelOf`), and the safe direction: a data train
/// that was really software would be read as landed before its image
/// rolled. An empty consist is software for the same reason.
///
/// Stamped on the train as `metadata.delivery_channel` at board, beside
/// `boarded_jobs`, so a reader can tell a config-only train from a
/// software one without opening every car (cffef553, 2026-09-15).
pub(crate) fn train_channel<'a>(cars: impl IntoIterator<Item = &'a Value>) -> &'static str {
    cars.into_iter()
        .map(|car| {
            car.get("metadata")
                .and_then(|m| m.get("delivery_channel"))
                .and_then(Value::as_str)
                .and_then(DeliveryChannel::parse)
                .unwrap_or(DeliveryChannel::Software)
        })
        .max()
        .unwrap_or(DeliveryChannel::Software)
        .label()
}

/// Weight one path. Higher is heavier to deliver. An unknown path is
/// treated as software — mis-routing a change too LIGHT is the dangerous
/// direction (a software change shipped as data is broken), too heavy is
/// only wasteful.
fn path_weight(path: &str) -> u8 {
    let p = path;
    // infra/hardware (4)
    if p.starts_with("infra/cluster/talos/") || p.contains("/talos/") {
        return 4;
    }
    // software (3)
    if p.starts_with("crates/") || p.starts_with("apps/") {
        return 3;
    }
    // config (2): deployed, but no build
    if p.starts_with("infra/cluster/manifests/")
        || p.ends_with(".service")
        || p.starts_with(".forgejo/")
        || p.starts_with(".github/")
        || p.starts_with("infra/lint/")
        || p.starts_with("infra/gate")
        || p.ends_with("Dockerfile")
        || p.ends_with("install.sh")
    {
        return 2;
    }
    // data (1): registry rows / docs / seeds — no build, no deploy
    if p.starts_with("infra/postgres/schema/")
        || p.contains("/seeds/")
        || p.ends_with("workflows.toml")
        || p.starts_with("infra/platform/workflows/")
        || p.starts_with("infra/dispatcher/rules/")
        || p.ends_with("rules.toml")
        || p.ends_with("-registry.sql")
        || p.starts_with("docs/")
        || p.ends_with(".md")
        || p.contains("/content/")
    {
        return 1;
    }
    // unknown → software (conservative)
    3
}

/// The delivery channel for a set of changed paths: the heaviest wins.
/// An empty set is Data (nothing to build or deploy).
pub(crate) fn delivery_channel(paths: &[String]) -> DeliveryChannel {
    let w = paths.iter().map(|p| path_weight(p)).max().unwrap_or(1);
    match w {
        4 => DeliveryChannel::Infra,
        3 => DeliveryChannel::Software,
        2 => DeliveryChannel::Config,
        _ => DeliveryChannel::Data,
    }
}

/// The delivery channel a branch would ship on, as a label — for
/// stamping on a gate-run so the car (and later the channel-gated
/// delivery) can branch on it without re-deriving. `None` when the
/// branch has no forge diff to classify (already merged / not pushed).
pub(crate) fn delivery_channel_for(branch: &str) -> Option<String> {
    changed_paths_for(branch).map(|paths| delivery_channel(&paths).label().to_string())
}

// ---- the tiers a change touched (ba429e7f, design 01c3cc3f) ---------------
//
// `software` is one delivery channel, and the question the consolidation
// period asks — is the core settling while work moves outward — had no
// reading in it (David, 2026-09-18). Beside the channel, a change now
// records the SET of tiers it touched, read off the one tier map
// (`infra/platform/tiers.toml`, through `boss_core::tiers`): a car
// touching crates/core and apps/ is {core, frontend}, and its headline
// is the lowest rank, core — the way the channel is the heaviest path.
// A path no tier claims (a root file) is left out, so a root-only change
// is the empty set: a reading, not a guess.

/// The tiers a set of changed paths touched, sorted by name.
pub(crate) fn software_tiers(paths: &[String]) -> Vec<String> {
    boss_core::tiers::tier_map()
        .map(|m| m.tiers_of(paths.iter().map(String::as_str)))
        .unwrap_or_default()
}

/// The headline tier among a set: the lowest rank (core over
/// frontend; ties by the map's row order). `None` for the empty set.
pub(crate) fn software_tier(tiers: &[String]) -> Option<String> {
    boss_core::tiers::tier_map()
        .ok()?
        .headline(tiers.iter().map(String::as_str))
        .map(|t| t.name.clone())
}

/// Both tier keys as the metadata map every stamp site merges — the
/// set under `software_tiers`, the headline under `software_tier`
/// (omitted for the empty set: absent, never null, since the metadata
/// door deletes a null key).
pub(crate) fn tier_stamps(paths: &[String]) -> serde_json::Map<String, Value> {
    let tiers = software_tiers(paths);
    let mut m = serde_json::Map::new();
    if let Some(head) = software_tier(&tiers) {
        m.insert(boss_jobs::car::SOFTWARE_TIER.to_string(), json!(head));
    }
    m.insert(boss_jobs::car::SOFTWARE_TIERS.to_string(), json!(tiers));
    m
}

/// The tier stamps for a branch's forge diff — empty (stamping
/// nothing, stripping nothing) when the diff will not resolve, the
/// same rule `delivery_channel_for` answers `None` by.
pub(crate) fn tier_stamps_for(branch: &str) -> serde_json::Map<String, Value> {
    changed_paths_for(branch)
        .map(|paths| tier_stamps(&paths))
        .unwrap_or_default()
}

/// The car with no stamp, named in a train's counts rather than
/// guessed into a tier: a car parked before the stamp existed is an
/// honest hole in the reading, the way `InputChannel::Unclassified` is.
pub(crate) const UNCLASSIFIED_TIER: &str = "unclassified";

/// A TRAIN's tiers from its cars' stamps: the union (sorted) and the
/// number of cars touching each tier — stamped on the train at arrival
/// as `software_tiers` / `software_tier_counts`, beside the arrival
/// report, so the series has one row per train without opening every
/// car. A car carrying no set counts under [`UNCLASSIFIED_TIER`] and
/// adds nothing to the union.
pub(crate) fn train_tiers<'a>(
    cars: impl IntoIterator<Item = &'a Value>,
) -> (Vec<String>, BTreeMap<String, usize>) {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut union: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for car in cars {
        match car
            .pointer(&format!("/metadata/{}", boss_jobs::car::SOFTWARE_TIERS))
            .and_then(Value::as_array)
        {
            Some(set) => {
                for tier in set.iter().filter_map(Value::as_str) {
                    *counts.entry(tier.to_string()).or_insert(0) += 1;
                    union.insert(tier.to_string());
                }
            }
            None => *counts.entry(UNCLASSIFIED_TIER.to_string()).or_insert(0) += 1,
        }
    }
    (union.into_iter().collect(), counts)
}

/// The per-tier reading over a window of landed trains — a STANDING
/// reading the platform retro quotes every week (backlog 79fdc808,
/// design 32f18167), so "is the core settling" is answered from the
/// record rather than argued.
///
/// Two rules are the point of the shape. It STATES ITS COVERAGE: how
/// many trains in the window carry a stamp, beside the window. On
/// 2026-09-19 56 of 67 landed trains carried none and the old mix
/// printed `unclassified 83.6%` beside confident per-tier percentages,
/// so the series built to answer the question could not answer it. A
/// share is therefore computed over the STAMPED trains only, and when
/// they are not a majority of the window no share is printed at all —
/// the rule 367b2dbe applied to the input half of this verb. And it
/// names its DIRECTION, outward, with NO TARGET RATIO: David's call,
/// recorded so it is not re-argued — a number picked to be moved
/// toward is managed rather than meant. Nothing here compares a share
/// against a threshold, and nothing should.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TierReading {
    /// Landed trains in the window.
    pub(crate) window: usize,
    /// Of those, the ones carrying `software_tiers` — the empty set
    /// (a root-only train) is a stamp; a missing key is not.
    pub(crate) stamped: usize,
    /// Stamped trains touching each tier (a train counts once per tier).
    pub(crate) mix: BTreeMap<String, usize>,
}

impl TierReading {
    pub(crate) fn of<'a>(trains: impl IntoIterator<Item = &'a Value>) -> TierReading {
        trains.into_iter().fold(
            TierReading {
                window: 0,
                stamped: 0,
                mix: BTreeMap::new(),
            },
            |mut r, train| {
                r.window += 1;
                if let Some(set) = train
                    .pointer(&format!("/metadata/{}", boss_jobs::car::SOFTWARE_TIERS))
                    .and_then(Value::as_array)
                {
                    r.stamped += 1;
                    for tier in set.iter().filter_map(Value::as_str) {
                        *r.mix.entry(tier.to_string()).or_insert(0) += 1;
                    }
                }
                r
            },
        )
    }

    /// Whether the stamped trains are MORE than half the window — the
    /// bar under which a share would describe the stamped few as if
    /// they were the whole. Exactly half is not a majority.
    pub(crate) fn covers_a_majority(&self) -> bool {
        self.stamped * 2 > self.window
    }

    /// The share of STAMPED trains touching `tier`, or `None` when the
    /// stamps do not cover a majority of the window.
    pub(crate) fn share(&self, tier: &str) -> Option<f64> {
        self.covers_a_majority()
            .then(|| self.mix.get(tier).copied().unwrap_or(0) as f64 / self.stamped as f64)
    }

    /// The reading as printed lines — coverage first, then the tiers in
    /// the map's rank order (then any tier the map no longer lists),
    /// then the direction. Pure, so the withholding rule is tested
    /// without an API.
    pub(crate) fn lines(&self) -> Vec<String> {
        if self.window == 0 {
            return vec![
                "  coverage: no landed train in the window — nothing to read, and that is the reading"
                    .to_string(),
                DIRECTION_LINE.to_string(),
            ];
        }
        let mut out = vec![format!(
            "  coverage: {} of {} landed train(s) carry a tier stamp ({:.0}%)",
            self.stamped,
            self.window,
            self.stamped as f64 * 100.0 / self.window as f64
        )];
        if !self.covers_a_majority() {
            out.push(format!(
                "  shares withheld: {} of {} is not a majority of the window, so a percentage \
                 would describe the stamped few as if they were the whole — counts only",
                self.stamped, self.window
            ));
        }
        let unstamped = self.window - self.stamped;
        if unstamped > 0 {
            out.push(format!(
                "  {unstamped} unstamped: boss channels --backfill-tiers reads them off their merge commits"
            ));
        }
        let order: Vec<String> = boss_core::tiers::tier_map()
            .map(|m| m.tiers.iter().map(|t| t.name.clone()).collect())
            .unwrap_or_default();
        let unknown: Vec<String> = self
            .mix
            .keys()
            .filter(|t| !order.contains(t))
            .cloned()
            .collect();
        out.extend(order.iter().chain(unknown.iter()).map(|tier| {
            let n = self.mix.get(tier).copied().unwrap_or(0);
            let pct = self
                .share(tier)
                .map(|s| format!("{:>5.1}%", s * 100.0))
                .unwrap_or_else(|| "    —".to_string());
            format!("    {tier:<14} {n:>4}  {pct}")
        }));
        out.push(DIRECTION_LINE.to_string());
        out
    }
}

/// The direction the reading is read in, printed with every reading so
/// a reader never supplies a target of their own (design 32f18167).
const DIRECTION_LINE: &str = "  direction: OUTWARD — work moving from core to the outer tiers; \
     no target ratio, by decision (design 32f18167): read the change, not a gap to a number";

/// The landed trains in a window, read from the system of record: every
/// closed train that merged and closed on or after `since`. Paged on
/// `total`, because a limit is a page and not a filter.
async fn landed_tier_reading(
    http: &reqwest::Client,
    since: chrono::NaiveDate,
) -> Result<TierReading> {
    let trains = crate::train::list_all_pages(|offset| {
        let http = http.clone();
        async move {
            crate::gate::api(
                &http,
                reqwest::Method::GET,
                &format!(
                    "/api/jobs?kind=pr-train&status=closed&limit={}&offset={offset}",
                    crate::train::PAGE_LIMIT
                ),
                None,
            )
            .await
        }
    })
    .await?;
    Ok(TierReading::of(
        trains
            .iter()
            .filter(|t| merge_ref_of(t).is_some())
            .filter(|t| {
                t.get("closed_on")
                    .and_then(Value::as_str)
                    .and_then(|d| d.parse::<chrono::NaiveDate>().ok())
                    .is_some_and(|closed| closed >= since)
            }),
    ))
}

fn print_landed(since: chrono::NaiveDate, reading: &TierReading) {
    println!(
        "\n  TIERS over landed trains since {since} — {} train(s) (a train counts once per tier)",
        reading.window
    );
    for line in reading.lines() {
        println!("{line}");
    }
}

/// `boss channels --tiers [--since <date>]` — the landed-train tier
/// reading alone, the form the platform retro's `collect` step quotes
/// into its `tier_mix` field every week (79fdc808). Reads only the
/// system of record: no git, no dock.
pub async fn tiers(
    since: Option<chrono::NaiveDate>,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<()> {
    let since =
        since.unwrap_or_else(|| (now - chrono::Duration::days(TIER_WINDOW_DAYS)).date_naive());
    let reading = landed_tier_reading(&reqwest::Client::new(), since).await?;
    println!("boss channels --tiers — is the core settling while work moves outward");
    print_landed(since, &reading);
    Ok(())
}

/// The merge ref a closed train records — its arrival report's
/// `merged_sha`, or the merged step's `merge_ref` (12 characters; the
/// caller resolves it). `None` for a train that never merged.
pub(crate) fn merge_ref_of(train: &Value) -> Option<&str> {
    train
        .pointer("/metadata/arrival_report/merged_sha")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .or_else(|| {
            boss_jobs::car::find_step(train, "merged", "Merged into main")
                .and_then(|s| s.pointer("/metadata/merge_ref"))
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
        })
}

/// Whether a closed train's tiers are still to be read: it has no
/// `software_tiers`, it merged, and — with a `since` date — it closed
/// on or after that date. The pure half of `--backfill-tiers`, so the
/// skip (idempotence) and the window are tested without git or HTTP.
pub(crate) fn needs_tier_backfill(train: &Value, since: Option<chrono::NaiveDate>) -> bool {
    if train
        .pointer(&format!("/metadata/{}", boss_jobs::car::SOFTWARE_TIERS))
        .is_some()
    {
        return false;
    }
    if merge_ref_of(train).is_none() {
        return false;
    }
    match since {
        None => true,
        Some(since) => train
            .get("closed_on")
            .and_then(Value::as_str)
            .and_then(|d| d.parse::<chrono::NaiveDate>().ok())
            .is_some_and(|closed| closed >= since),
    }
}

/// The paths a merge commit changed against its first parent, from
/// `git show --name-only --format= --first-parent <sha>` — one path per
/// line, which is what makes this parse a filter and not a grammar.
/// (`--stat` prints the same files with a histogram and wraps a long
/// path in `{a => b}`; the name-only form is the same command's
/// answer without either.)
pub(crate) fn paths_of_name_only(output: &str) -> Vec<String> {
    output
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect()
}

/// The tier stamp a backfill writes on a closed train: the set read
/// from its merge commit, and where it came from — so a row the arrival
/// stamped live and one read back from git are told apart.
pub(crate) fn backfill_patch(paths: &[String], merge_sha: &str) -> Value {
    let mut m = tier_stamps(paths);
    m.insert(
        "software_tiers_source".to_string(),
        json!(format!("merge-commit {merge_sha}")),
    );
    Value::Object(m)
}

/// A merge commit's changed paths, from the checkout. A 12-character
/// `merge_ref` is resolved by git itself; `None` when the commit is not
/// in this checkout (never fetched here — the caller's checkout is the
/// record) or the diff is empty.
fn merge_commit_paths(merge_ref: &str) -> Option<Vec<String>> {
    let out = std::process::Command::new("git")
        .args([
            "show",
            "--name-only",
            "--format=",
            "--first-parent",
            merge_ref,
        ])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let paths = paths_of_name_only(&String::from_utf8_lossy(&out.stdout));
    if paths.is_empty() { None } else { Some(paths) }
}

/// Print one per-tier mix: `<tier> <n> <pct>`, over `total` rows.
fn print_tier_mix(mix: &BTreeMap<String, usize>, total: usize) {
    // Rank order, as the map lists them, then anything the map does
    // not know (an unclassified count, a tier a later map dropped).
    let order: Vec<String> = boss_core::tiers::tier_map()
        .map(|m| m.tiers.iter().map(|t| t.name.clone()).collect())
        .unwrap_or_default();
    let known = order
        .iter()
        .filter_map(|t| mix.get(t).map(|n| (t.clone(), *n)));
    let unknown = mix
        .iter()
        .filter(|(t, _)| !order.contains(t))
        .map(|(t, n)| (t.clone(), *n));
    for (tier, n) in known.chain(unknown) {
        let pct = if total > 0 {
            (n as f64) * 100.0 / (total as f64)
        } else {
            0.0
        };
        println!("    {:<14} {:>4}  {:>5.1}%", tier, n, pct);
    }
}

/// `boss channels --backfill-tiers [--since <date>]`: read each closed
/// train's merge commit in this checkout and PATCH `software_tiers`
/// onto the train. Idempotent — a train carrying the key is skipped —
/// so the verb can be re-run after every convergence without
/// rewriting what an arrival stamped live.
pub async fn backfill_tiers(since: Option<chrono::NaiveDate>) -> Result<()> {
    let http = reqwest::Client::new();
    let trains = crate::train::list_all_pages(|offset| {
        let http = http.clone();
        async move {
            crate::gate::api(
                &http,
                reqwest::Method::GET,
                &format!(
                    "/api/jobs?kind=pr-train&status=closed&limit={}&offset={offset}",
                    crate::train::PAGE_LIMIT
                ),
                None,
            )
            .await
        }
    })
    .await?;
    let (mut written, mut skipped, mut unreadable) = (0usize, 0usize, 0usize);
    for train in &trains {
        if !needs_tier_backfill(train, since) {
            skipped += 1;
            continue;
        }
        let id = train.get("id").and_then(Value::as_str).unwrap_or("?");
        let merge_ref = merge_ref_of(train).unwrap_or_default();
        let Some(paths) = merge_commit_paths(merge_ref) else {
            println!(
                "  {}  {merge_ref}: not in this checkout or an empty diff — left unread",
                &id[..8.min(id.len())]
            );
            unreadable += 1;
            continue;
        };
        let patch = backfill_patch(&paths, merge_ref);
        crate::gate::api(
            &http,
            reqwest::Method::PATCH,
            &format!("/api/jobs/{id}/metadata"),
            Some(patch.clone()),
        )
        .await?;
        println!(
            "  {}  {merge_ref}: {}",
            &id[..8.min(id.len())],
            patch
                .get(boss_jobs::car::SOFTWARE_TIERS)
                .map(|v| v.to_string())
                .unwrap_or_default()
        );
        written += 1;
    }
    println!(
        "boss channels --backfill-tiers: {} closed train(s) read; {written} stamped, \
         {skipped} skipped (already stamped, never merged, or outside --since), \
         {unreadable} unreadable here",
        trains.len()
    );
    Ok(())
}

/// A car's changed files, best-effort, from the forge ref against
/// origin/main. Returns None when git cannot resolve the branch (a
/// merged car whose branch is gone) so the caller can skip rather than
/// miscount.
pub(crate) fn changed_paths_for(branch: &str) -> Option<Vec<String>> {
    let out = std::process::Command::new("git")
        .args([
            "diff",
            "--name-only",
            &format!("origin/main...origin/{branch}"),
        ])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let files: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();
    if files.is_empty() { None } else { Some(files) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn job(kind: &str, md: Value) -> Value {
        json!({ "kind": kind, "metadata": md })
    }

    #[test]
    fn user_feedback_kind_is_the_feedback_lane() {
        assert_eq!(
            input_channel(&job("user-feedback", json!({}))),
            InputChannel::UserFeedback
        );
    }

    #[test]
    fn maintenance_and_rotation_are_scheduled() {
        assert_eq!(
            input_channel(&job("maintenance-backup", json!({}))),
            InputChannel::Scheduled
        );
        assert_eq!(
            input_channel(&job("rotate-a-credential", json!({}))),
            InputChannel::Scheduled
        );
    }

    #[test]
    fn david_files_roadmap_claude_files_discovery() {
        assert_eq!(
            input_channel(&job(
                "backlog-item",
                json!({"reporter":"David 2026-09-03","title":"forge topology"})
            )),
            InputChannel::Roadmap
        );
        assert_eq!(
            input_channel(&job(
                "backlog-item",
                json!({"reporter":"claude@algedonic.dev","title":"some fix found mid-job"})
            )),
            InputChannel::Discovery
        );
    }

    #[test]
    fn specific_signals_win_over_reporter() {
        // A claude-filed post-mortem is PostMortem, not Discovery.
        assert_eq!(
            input_channel(&job(
                "backlog-item",
                json!({"reporter":"claude@algedonic.dev","title":"Post-mortem: 2026-09-02 boot-brick outages (5 Whys)"})
            )),
            InputChannel::PostMortem
        );
        // A claude-filed red-train item is PipelineFailure.
        assert_eq!(
            input_channel(&job(
                "backlog-item",
                json!({"reporter":"claude@algedonic.dev","title":"Every CI job fetch reddens a whole train"})
            )),
            InputChannel::PipelineFailure
        );
        // A monitoring/disk item is Telemetry.
        assert_eq!(
            input_channel(&job(
                "backlog-item",
                json!({"reporter":"claude@algedonic.dev","area":"cluster","title":"forge disk at the floor, estate observer silent"})
            )),
            InputChannel::Telemetry
        );
    }

    #[test]
    fn an_unattributed_item_is_named_not_guessed() {
        assert_eq!(
            input_channel(&job(
                "backlog-item",
                json!({"title":"credentials become protocol"})
            )),
            InputChannel::Unclassified
        );
    }

    #[test]
    fn mix_counts_and_proactive_share() {
        let jobs = vec![
            job("user-feedback", json!({})), // proactive
            job(
                "backlog-item",
                json!({"reporter":"David","title":"a feature"}),
            ), // roadmap: proactive
            job(
                "backlog-item",
                json!({"reporter":"claude","title":"a bug found"}),
            ), // discovery: reactive
            job("backlog-item", json!({"title":"post-mortem 5-whys"})), // reactive
        ];
        let mix = input_mix(&jobs);
        assert_eq!(mix.get(&InputChannel::UserFeedback), Some(&1));
        assert_eq!(mix.get(&InputChannel::Roadmap), Some(&1));
        assert_eq!(mix.get(&InputChannel::Discovery), Some(&1));
        assert_eq!(mix.get(&InputChannel::PostMortem), Some(&1));
        // 2 proactive of 4.
        assert_eq!(proactive_share(&mix), Some(0.5));
    }

    #[test]
    fn proactive_share_is_none_on_empty() {
        assert_eq!(proactive_share(&BTreeMap::new()), None);
    }

    /// The FIX for c5dc81a1: a lane the filer WROTE is read off the
    /// packet, not guessed from its words. A title carrying none of the
    /// keywords still classifies, because the filer said so.
    #[test]
    fn a_recorded_channel_is_read_not_guessed() {
        let j = json!({
            "kind": "backlog-item",
            "metadata": {
                "input_channel": "discovery-while-working",
                "title": "deny_unknown_fields is inert under serde flatten",
                "opened_at": "2026-09-25T00:00:00Z",
            },
        });
        assert_eq!(
            classify(&j),
            (InputChannel::Discovery, Provenance::Recorded)
        );
        // And the recorded lane WINS over a keyword the heuristic would
        // have matched: the filer knows the origin, the words do not.
        let j = json!({
            "kind": "backlog-item",
            "metadata": {
                "input_channel": "roadmap",
                "reporter": "claude@algedonic.dev",
                "title": "forge disk at the floor",
                "opened_at": "2026-09-25T00:00:00Z",
            },
        });
        assert_eq!(classify(&j), (InputChannel::Roadmap, Provenance::Recorded));
    }

    /// The era boundary (c5dc81a1, the agent_runs handling): rows filed
    /// BEFORE the field existed keep the keyword inference, marked as
    /// inference; rows filed after it without one are `unclassified`
    /// meaning the filer did not say — which is actionable.
    #[test]
    fn the_pre_field_era_keeps_the_guess_and_the_new_era_does_not() {
        let old = json!({
            "kind": "backlog-item",
            "metadata": {
                "reporter": "claude@algedonic.dev",
                "title": "a bug found mid-car",
                "opened_at": "2026-09-19T03:55:29Z",
            },
        });
        assert_eq!(
            classify(&old),
            (InputChannel::Discovery, Provenance::Inferred)
        );
        let new = json!({
            "kind": "backlog-item",
            "metadata": {
                "reporter": "claude@algedonic.dev",
                "title": "a bug found mid-car",
                "opened_at": "2026-09-25T03:55:29Z",
            },
        });
        assert_eq!(
            classify(&new),
            (InputChannel::Unclassified, Provenance::NotSaid)
        );
    }

    /// A lane spelled wrong is not silently a guess: an unreadable
    /// recorded value falls back to the same reading as no value, so a
    /// typo can never masquerade as a fact.
    #[test]
    fn an_unreadable_recorded_lane_is_not_a_fact() {
        let j = json!({
            "kind": "backlog-item",
            "metadata": {"input_channel": "vibes", "opened_at": "2026-09-25T00:00:00Z"},
        });
        assert_eq!(
            classify(&j),
            (InputChannel::Unclassified, Provenance::NotSaid)
        );
    }

    /// `unclassified` is what the ABSENCE of an answer reads as, so it
    /// is not in the vocabulary a filer may name.
    #[test]
    fn the_fileable_vocabulary_excludes_unclassified() {
        assert_eq!(
            InputChannel::parse("discovery-while-working"),
            Some(InputChannel::Discovery)
        );
        assert_eq!(InputChannel::parse("unclassified"), None);
        assert_eq!(InputChannel::parse("nonsense"), None);
        // One list, so the help text and the parser cannot drift (§9a).
        for lane in FILEABLE_LANES {
            assert_eq!(
                InputChannel::parse(lane.label()),
                Some(lane),
                "{}",
                lane.label()
            );
        }
        assert!(!FILEABLE_LANES.contains(&InputChannel::Unclassified));
    }

    /// 367b2dbe, retired: the headline used to be computed over the
    /// minority the keywords matched and printed with no caveat. It is
    /// now computed over RECORDED rows only, and is `n/a` when nothing
    /// recorded a lane — a number over a guess is worse than no number.
    #[test]
    fn the_proactive_headline_counts_only_recorded_lanes() {
        let jobs = vec![
            // Pre-field era, inferred roadmap — NOT a recorded fact.
            job(
                "backlog-item",
                json!({"reporter":"David","title":"a feature","opened_at":"2026-09-01T00:00:00Z"}),
            ),
            // New era, filer said nothing.
            job(
                "backlog-item",
                json!({"title":"x","opened_at":"2026-09-25T00:00:00Z"}),
            ),
        ];
        let p = provenances(&jobs);
        assert_eq!((p.recorded, p.inferred, p.not_stated), (0, 1, 1));
        assert_eq!(proactive_share(&recorded_mix(&jobs)), None);

        // One recorded roadmap row and one recorded discovery row: the
        // share is 50% over TWO, not over the four rows.
        let mut jobs = jobs;
        jobs.push(job(
            "backlog-item",
            json!({"input_channel":"roadmap","opened_at":"2026-09-25T00:00:00Z"}),
        ));
        jobs.push(job(
            "backlog-item",
            json!({"input_channel":"discovery-while-working","opened_at":"2026-09-25T00:00:00Z"}),
        ));
        let p = provenances(&jobs);
        assert_eq!((p.recorded, p.inferred, p.not_stated), (2, 1, 1));
        assert_eq!(proactive_share(&recorded_mix(&jobs)), Some(0.5));
        // The full mix still counts every row, so nothing goes missing.
        assert_eq!(input_mix(&jobs).values().sum::<usize>(), 4);
    }

    /// A packet whose KIND names its lane needs no field: the kind is
    /// already a recorded fact, not vocabulary overlap.
    #[test]
    fn a_kind_that_names_its_lane_is_recorded_by_the_kind() {
        assert_eq!(
            classify(&job(
                "user-feedback",
                json!({"opened_at":"2026-09-25T00:00:00Z"})
            )),
            (InputChannel::UserFeedback, Provenance::Recorded)
        );
        assert_eq!(
            classify(&job(
                "maintenance-backup",
                json!({"opened_at":"2026-09-25T00:00:00Z"})
            )),
            (InputChannel::Scheduled, Provenance::Recorded)
        );
    }

    #[test]
    fn delivery_channel_classifies_by_artifact() {
        assert_eq!(
            delivery_channel(&["infra/postgres/schema/2026-x.sql".into()]),
            DeliveryChannel::Data
        );
        assert_eq!(
            delivery_channel(&["infra/platform/workflows/ship-a-change.toml".into()]),
            DeliveryChannel::Data
        );
        assert_eq!(
            delivery_channel(&[".forgejo/workflows/ci.yml".into()]),
            DeliveryChannel::Config
        );
        assert_eq!(
            delivery_channel(&["crates/core/boss-jobs/src/lib.rs".into()]),
            DeliveryChannel::Software
        );
        assert_eq!(
            delivery_channel(&["infra/cluster/talos/w-1.yaml".into()]),
            DeliveryChannel::Infra
        );
    }

    #[test]
    fn delivery_channel_is_the_heaviest_of_a_mixed_car() {
        // a registry row PLUS a crate still needs the build → software
        assert_eq!(
            delivery_channel(&[
                "infra/dispatcher/rules/converge-on-merge.toml".into(),
                "crates/core/boss-dispatcher/src/x.rs".into(),
            ]),
            DeliveryChannel::Software
        );
        // config PLUS infra → infra
        assert_eq!(
            delivery_channel(&[
                ".forgejo/workflows/ci.yml".into(),
                "infra/cluster/talos/cp-1.yaml".into(),
            ]),
            DeliveryChannel::Infra
        );
    }

    #[test]
    fn an_unknown_path_is_conservative_software() {
        assert_eq!(
            delivery_channel(&["some/random/file.xyz".into()]),
            DeliveryChannel::Software
        );
    }

    #[test]
    fn an_empty_change_is_data() {
        assert_eq!(delivery_channel(&[]), DeliveryChannel::Data);
    }

    // ---- the tiers a change touched (ba429e7f) ----------------------------

    fn paths(ps: &[&str]) -> Vec<String> {
        ps.iter().map(|p| p.to_string()).collect()
    }

    #[test]
    fn a_car_touching_core_and_the_frontend_is_both_with_core_as_the_headline() {
        let p = paths(&[
            "crates/core/boss-jobs/src/car.rs",
            "apps/web/src/it/yard/yard.ts",
            "apps/web/src/it/yard/yard-production.ts",
        ]);
        assert_eq!(software_tiers(&p), ["core", "frontend"]);
        assert_eq!(software_tier(&software_tiers(&p)).as_deref(), Some("core"));
        let stamps = tier_stamps(&p);
        assert_eq!(stamps["software_tiers"], json!(["core", "frontend"]));
        assert_eq!(stamps["software_tier"], "core");
        // Beside, not instead of: the delivery channel still reads software.
        assert_eq!(delivery_channel(&p), DeliveryChannel::Software);
    }

    #[test]
    fn a_docs_only_car_is_data_and_a_root_only_car_is_the_empty_set() {
        let p = paths(&["docs/design/x.md", "docs/architecture-decisions.md"]);
        assert_eq!(software_tiers(&p), ["data"]);
        assert_eq!(software_tier(&software_tiers(&p)).as_deref(), Some("data"));
        // A file no tier claims: the set is empty and there is no
        // headline — stamped as the empty set (a reading), with the
        // headline key absent (never null).
        let root = paths(&["README.md", "Cargo.toml"]);
        assert!(software_tiers(&root).is_empty());
        let stamps = tier_stamps(&root);
        assert_eq!(stamps["software_tiers"], json!([]));
        assert!(!stamps.contains_key("software_tier"));
    }

    #[test]
    fn a_platform_registry_row_is_data_even_though_it_lives_under_infra() {
        let p = paths(&[
            "infra/platform/workflows/ship-a-change.toml",
            "infra/gate.sh",
        ]);
        assert_eq!(software_tiers(&p), ["data", "infra"]);
        // infra is rank 1, data rank 4: the headline is infra.
        assert_eq!(software_tier(&software_tiers(&p)).as_deref(), Some("infra"));
    }

    #[test]
    fn a_train_aggregates_its_cars_tiers_and_names_an_unstamped_car() {
        let cars = [
            json!({ "metadata": { "software_tiers": ["core", "frontend"] } }),
            json!({ "metadata": { "software_tiers": ["frontend"] } }),
            json!({ "metadata": { "software_tiers": [] } }),
            json!({ "metadata": {} }),
        ];
        let (union, counts) = train_tiers(cars.iter());
        assert_eq!(union, ["core", "frontend"]);
        assert_eq!(counts.get("core"), Some(&1));
        assert_eq!(counts.get("frontend"), Some(&2));
        assert_eq!(counts.get(UNCLASSIFIED_TIER), Some(&1));
        // The empty set is a stamp: it is not unclassified.
        assert_eq!(counts.values().sum::<usize>(), 4);
    }

    #[test]
    fn the_landed_reading_counts_trains_per_tier_over_the_stamped_and_states_its_coverage() {
        let trains = [
            json!({ "metadata": { "software_tiers": ["core", "data"] } }),
            json!({ "metadata": { "software_tiers": ["data"] } }),
            json!({ "metadata": { "software_tiers": [] } }),
            json!({ "metadata": { "delivery_channel": "software" } }),
        ];
        let r = TierReading::of(trains.iter());
        assert_eq!(r.window, 4);
        // The empty set is a stamp (a root-only train); a missing key is not.
        assert_eq!(r.stamped, 3);
        assert_eq!(r.mix.get("core"), Some(&1));
        assert_eq!(r.mix.get("data"), Some(&2));
        // The unstamped train is COVERAGE, not a tier: it is not in the mix.
        assert_eq!(r.mix.get(UNCLASSIFIED_TIER), None);
        assert!(r.covers_a_majority());
        // Shares are over the stamped trains, never over the window.
        assert_eq!(r.share("data"), Some(2.0 / 3.0));
        assert_eq!(r.share("frontend"), Some(0.0));
        let text = r.lines().join("\n");
        assert!(
            text.contains("coverage: 3 of 4 landed train(s) carry a tier stamp (75%)"),
            "{text}"
        );
        assert!(text.contains("66.7%"), "{text}");
    }

    /// The trap the whole series nearly failed on (79fdc808): on
    /// 2026-09-19 56 of 67 landed trains carried no stamp and the old
    /// mix printed "unclassified 83.6%" beside confident per-tier
    /// percentages. A reading over a minority says so and prints no
    /// share at all — the same rule 367b2dbe applied to the input half.
    #[test]
    fn a_reading_over_a_minority_of_the_window_withholds_every_share() {
        let mut trains: Vec<Value> = (0..56).map(|_| json!({ "metadata": {} })).collect();
        trains.extend((0..11).map(|_| json!({ "metadata": { "software_tiers": ["core"] } })));
        let r = TierReading::of(trains.iter());
        assert_eq!((r.window, r.stamped), (67, 11));
        assert!(!r.covers_a_majority());
        assert_eq!(r.share("core"), None);
        let text = r.lines().join("\n");
        assert!(
            text.contains("coverage: 11 of 67 landed train(s) carry a tier stamp (16%)"),
            "{text}"
        );
        assert!(text.contains("shares withheld"), "{text}");
        assert!(
            !text.contains("100.0%"),
            "no percentage over the minority: {text}"
        );
        assert!(text.contains("--backfill-tiers"), "{text}");
        // Exactly half is not a majority either.
        let half = [
            json!({ "metadata": { "software_tiers": ["core"] } }),
            json!({ "metadata": {} }),
        ];
        assert_eq!(TierReading::of(half.iter()).share("core"), None);
    }

    /// The direction is named and no target is: David, design 32f18167 —
    /// a number picked to be moved toward is managed rather than meant.
    #[test]
    fn the_reading_names_its_direction_outward_and_no_target_ratio() {
        let trains = [json!({ "metadata": { "software_tiers": ["modules"] } })];
        let text = TierReading::of(trains.iter()).lines().join("\n");
        assert!(text.contains("direction: OUTWARD"), "{text}");
        assert!(text.contains("no target ratio"), "{text}");
        let empty = TierReading::of(std::iter::empty::<&Value>());
        assert_eq!((empty.window, empty.stamped), (0, 0));
        assert_eq!(empty.share("core"), None);
        assert!(
            empty
                .lines()
                .join("\n")
                .contains("no landed train in the window"),
            "an empty window is a reading, said as one"
        );
    }

    // ---- the backfill's pure parts ----------------------------------------

    fn closed_train(merge_ref: Option<&str>, closed_on: &str, stamped: bool) -> Value {
        let mut t = json!({
            "id": "t",
            "status": "closed",
            "closed_on": closed_on,
            "metadata": {},
            "steps": [],
        });
        if let Some(r) = merge_ref {
            t["steps"] = json!([{ "spec_slug": "merged", "title": "DEPARTED — merged into main",
                                  "status": "completed", "metadata": { "merge_ref": r } }]);
        }
        if stamped {
            t["metadata"]["software_tiers"] = json!(["core"]);
        }
        t
    }

    #[test]
    fn the_merge_ref_is_read_off_the_arrival_report_or_the_merged_step() {
        let by_step = closed_train(Some("d284168bcd78"), "2026-09-19", false);
        assert_eq!(merge_ref_of(&by_step), Some("d284168bcd78"));
        let mut by_report = by_step.clone();
        by_report["metadata"]["arrival_report"] = json!({ "merged_sha": "d284168bcd78aaaa" });
        assert_eq!(merge_ref_of(&by_report), Some("d284168bcd78aaaa"));
        assert_eq!(merge_ref_of(&closed_train(None, "2026-09-19", false)), None);
    }

    #[test]
    fn the_backfill_skips_a_stamped_train_a_never_merged_one_and_one_outside_the_window() {
        let since = "2026-09-10".parse::<chrono::NaiveDate>().unwrap();
        // Idempotent: a train carrying the key is never rewritten.
        assert!(!needs_tier_backfill(
            &closed_train(Some("abc"), "2026-09-19", true),
            None
        ));
        // Never merged (cancelled): nothing to read.
        assert!(!needs_tier_backfill(
            &closed_train(None, "2026-09-19", false),
            None
        ));
        // Merged, unstamped: read it — inside the window or with none.
        assert!(needs_tier_backfill(
            &closed_train(Some("abc"), "2026-09-19", false),
            None
        ));
        assert!(needs_tier_backfill(
            &closed_train(Some("abc"), "2026-09-10", false),
            Some(since)
        ));
        assert!(!needs_tier_backfill(
            &closed_train(Some("abc"), "2026-09-09", false),
            Some(since)
        ));
    }

    #[test]
    fn a_name_only_diff_is_one_path_per_line_and_the_patch_names_its_source() {
        let out = "apps/web/src/a.ts\ncrates/core/boss-jobs/src/lib.rs\n\n";
        let paths = paths_of_name_only(out);
        assert_eq!(
            paths,
            ["apps/web/src/a.ts", "crates/core/boss-jobs/src/lib.rs"]
        );
        let patch = backfill_patch(&paths, "d284168bcd78");
        assert_eq!(patch["software_tiers"], json!(["core", "frontend"]));
        assert_eq!(patch["software_tier"], "core");
        assert_eq!(patch["software_tiers_source"], "merge-commit d284168bcd78");
    }

    // ---- the train's channel: the heaviest of its cars' (cffef553) ----

    /// A consist of cars stamped with the given labels (`None` = a car
    /// parked before the stamp existed), folded to the train's channel.
    fn train_of(stamps: &[Option<&str>]) -> &'static str {
        let cars: Vec<Value> = stamps
            .iter()
            .map(|s| match s {
                Some(ch) => json!({ "id": "car", "metadata": { "delivery_channel": ch } }),
                None => json!({ "id": "car", "metadata": {} }),
            })
            .collect();
        train_channel(cars.iter())
    }

    #[test]
    fn a_train_of_data_cars_is_a_data_train() {
        assert_eq!(train_of(&[Some("data"), Some("data")]), "data");
    }

    #[test]
    fn a_mixed_train_ships_on_its_heaviest_car() {
        // A registry row beside a crate still rides the image roll.
        assert_eq!(train_of(&[Some("data"), Some("software")]), "software");
        // data + config → config; config + infra → infra: the SAME order
        // a mixed car resolves on, not a second one.
        assert_eq!(train_of(&[Some("config"), Some("data")]), "config");
        assert_eq!(train_of(&[Some("infra"), Some("config")]), "infra");
    }

    #[test]
    fn an_unstamped_car_reads_as_software() {
        // A car parked before the gate stamped channels, or one whose
        // stamp names nothing this order knows, is the car's own default:
        // software — mis-routing LIGHT is the dangerous direction.
        assert_eq!(train_of(&[Some("data"), None]), "software");
        assert_eq!(train_of(&[Some("data"), Some("firmware")]), "software");
        assert_eq!(train_of(&[]), "software");
    }

    /// A LIMIT IS NOT A FILTER (memory: a-limit-is-not-a-filter). Once
    /// open cars fill more than a page, a boardable car sorts to the
    /// tail and the old bare `limit=200` read dropped it from the
    /// delivery mix silently. The read now pages on `total` (via
    /// `train::list_all_pages`, whose page-two behaviour is pinned
    /// there), so the boardable filter must count a car gathered from
    /// page two.
    #[tokio::test]
    async fn a_boardable_car_on_page_two_is_counted() {
        let mut all: Vec<Value> = (0..crate::train::PAGE_LIMIT + 40)
            .map(|i| json!({ "id": i, "steps": [] }))
            .collect();
        all.push(json!({
            "id": "tail",
            "steps": [{ "title": "Open for review", "status": "ready" }]
        }));
        let all_ref = &all;
        let gathered = crate::train::list_all_pages(|offset| async move {
            let page: Vec<Value> = all_ref
                .iter()
                .skip(offset)
                .take(crate::train::PAGE_LIMIT)
                .cloned()
                .collect();
            anyhow::Ok(Some(json!({ "data": page, "total": all_ref.len() })))
        })
        .await
        .unwrap();
        let boardable = boardable_cars(gathered);
        assert!(
            boardable
                .iter()
                .any(|c| c.get("id").and_then(Value::as_str) == Some("tail")),
            "the boardable car on page two must be counted, not dropped off page one"
        );
    }
}
