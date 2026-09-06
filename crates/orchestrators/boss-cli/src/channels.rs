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
    Ok(())
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
}
