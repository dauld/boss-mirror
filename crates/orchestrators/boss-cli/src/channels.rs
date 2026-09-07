//! Where the IT department's work comes from — the INPUT-channel mix.
//!
//! Design: docs/design/it-delivery-channels.md (approved 2026-09-06).
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
use serde_json::Value;

/// The lane a work-originating job entered through.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum InputChannel {
    UserFeedback,
    Roadmap,
    DesignResolution,
    Review,
    Telemetry,
    PipelineFailure,
    Discovery,
    PostMortem,
    Dependency,
    Scheduled,
    Unclassified,
}

impl InputChannel {
    pub(crate) fn label(self) -> &'static str {
        match self {
            InputChannel::UserFeedback => "user-feedback",
            InputChannel::Roadmap => "roadmap",
            InputChannel::DesignResolution => "design-resolution",
            InputChannel::Review => "review-finding",
            InputChannel::Telemetry => "telemetry/monitoring",
            InputChannel::PipelineFailure => "pipeline-failure",
            InputChannel::Discovery => "discovery-while-working",
            InputChannel::PostMortem => "post-mortem",
            InputChannel::Dependency => "dependency/external",
            InputChannel::Scheduled => "scheduled",
            InputChannel::Unclassified => "unclassified",
        }
    }

    /// Proactive lanes build what is wanted; the rest are the system
    /// responding to something that already happened. This split is the
    /// health reading, not a value judgement on the work itself.
    pub(crate) fn is_proactive(self) -> bool {
        matches!(
            self,
            InputChannel::UserFeedback | InputChannel::Roadmap | InputChannel::DesignResolution
        )
    }
}

fn field<'a>(job: &'a Value, key: &str) -> &'a str {
    job.get("metadata")
        .and_then(|m| m.get(key))
        .and_then(Value::as_str)
        .unwrap_or("")
}

/// Classify a work-originating job by its input lane. Pure: same job,
/// same lane. Order matters — most specific signal wins.
pub(crate) fn input_channel(job: &Value) -> InputChannel {
    let kind = job.get("kind").and_then(Value::as_str).unwrap_or("");
    if kind == "user-feedback" {
        return InputChannel::UserFeedback;
    }
    if kind.starts_with("maintenance-") || kind == "rotate-a-credential" {
        return InputChannel::Scheduled;
    }

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
/// input-channel mix with the proactive-vs-reactive reading.
pub async fn run() -> Result<()> {
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
    if uf_total > 0 {
        *mix.entry(InputChannel::UserFeedback).or_insert(0) += uf_total;
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
    match proactive_share(&mix) {
        Some(s) => println!(
            "\n  proactive share: {:.0}%  — {}",
            s * 100.0,
            if s >= 0.5 {
                "building what is wanted"
            } else {
                "tilted toward firefighting"
            }
        ),
        None => println!("\n  proactive share: n/a — no classified work in the window"),
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
            Some(paths) => *dmix.entry(delivery_channel(&paths)).or_insert(0) += 1,
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
    Ok(())
}

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

    #[test]
    fn delivery_channel_classifies_by_artifact() {
        assert_eq!(
            delivery_channel(&["infra/postgres/schema/2026-x.sql".into()]),
            DeliveryChannel::Data
        );
        assert_eq!(
            delivery_channel(&["infra/platform/workflows.toml".into()]),
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
                "infra/dispatcher/rules.toml".into(),
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
