//! THE PUBLISH REGION (design cb38d806, backlog eee42416): the crossing
//! out of the world, where what landed on main is proposed to the public
//! GitHub mirror.

use super::*;

/// THE PUBLISH PACKET'S KIND (design cb38d806). One `publish-to-github`
/// packet is one day's measurement of the drift between the forge and
/// the public mirror; the ones that found drift opened a pull request.
pub const PUBLISH_KIND: &str = "publish-to-github";

/// How long a mirror pull request may stand before the publish is
/// STALLED. Design cb38d806 §4, decided by David 2026-09-19: "a stalled
/// publish (open PR older than 24 h, or a red reading unjudged) looks
/// troubled on the surface". The target it serves is §1 — a publish PR
/// every day there is drift, sized like a day of trains — which a PR
/// left standing defeats: #239 carried 1319 files in one commit because
/// the publishes before it had never been merged, and the scan's own
/// footnote says a change that large reads as all-new code.
pub const STALLED_PUBLISH_HOURS: i64 = 24;

/// ONE MIRROR PULL REQUEST, as the publish packet recorded it — the
/// `open-pr` step's own fields, the `read-checks` reading, whether
/// `judge-checks` judged it, and what GitHub last answered about the
/// PR's state (`pr_state`). NOTHING HERE IS FETCHED FROM THE MIRROR:
/// the readback car (backlog 321f1409) put the scan on the packet, and
/// `publish-github-pr.sh --measure` puts the PR's state there (backlog
/// a5d4322c), precisely so a surface could read both with no
/// credential and no second opinion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishPr {
    /// The publish packet that opened it — what the publish region
    /// counts, in the partition ([`members`]).
    pub packet_id: String,
    /// The pull request, as the verb recorded it.
    pub url: String,
    /// The snapshot commit the PR proposes.
    pub snapshot: String,
    /// When `open-pr` completed: the instant the PR was opened.
    pub opened: Instant,
    /// The scan's conclusion as the mirror spells it (`success`,
    /// `failure`, `absent`…). EMPTY when `read-checks` has not
    /// completed — which is not a pass, and is why [`Self::unjudged_red`]
    /// asks for a conclusion that was actually read.
    pub conclusion: String,
    /// The reading's own numbers, as the step recorded them — the
    /// region says the reading, never a symptom (design cb38d806 §4).
    pub alerts: String,
    pub rules: String,
    /// The checks the reading saw complete without passing, as the
    /// step recorded them (`<name>: <conclusion>`, `; `-joined) — the
    /// mirror's Gate among them since backlog c6cb678b. EMPTY when
    /// none failed, or on a reading older than that field.
    pub failing: String,
    /// `judge-checks` completed: a disposition per rule is on the
    /// packet. A SKIPPED judge is not a judgement — the workflow skips
    /// it only when the scan concluded `success`, which the conclusion
    /// already says.
    pub judged: bool,
    /// GitHub, asked, said this PR merged.
    pub merged: bool,
    /// GitHub, asked, said this PR is closed — merged or not.
    pub closed: bool,
    /// When GitHub was last asked about this PR. EMPTY when it never
    /// was, which is not "open": the region then says the state was
    /// never read rather than how long the PR has been open.
    pub state_read_at: String,
}

impl PublishPr {
    /// Still standing as far as the record knows: GitHub has not been
    /// read saying it closed. An UNREAD PR counts here — an unasked
    /// question is not a pass — and the region's sentence says which of
    /// the two it is.
    pub fn awaiting(&self) -> bool {
        !self.closed
    }

    /// A RED READING NOBODY JUDGED: the scan was read, it did not
    /// conclude `success`, and no disposition was recorded. This is
    /// design cb38d806 §2's defect exactly — "a red badge on the mirror
    /// with no reading in the system of record is the defect" — and PR
    /// #238 was merged over 64 unread alerts because nothing said so.
    pub fn unjudged_red(&self) -> bool {
        !self.conclusion.is_empty() && self.conclusion != "success" && !self.judged
    }

    /// The READING, in the words the region carries. An alarm that
    /// reports a symptom sends a human to re-derive what the system
    /// already recorded (CLAUDE.md §Diagnosis), so the sentence names
    /// the conclusion and both counts the step wrote down.
    ///
    /// A FAILING CHECK IS NAMED (backlog c6cb678b): PR #243's Gate
    /// failed, and the only sentence the region ever carried about it
    /// was the PR's age. A verdict must name what failed.
    pub fn reading(&self) -> String {
        let failing = if self.failing.is_empty() {
            String::new()
        } else {
            format!(" — failing: {}", self.failing)
        };
        format!(
            "the checks read {} — {} alert(s) over {} rule(s){failing}, no disposition recorded",
            self.conclusion, self.alerts, self.rules
        )
    }
}

/// The pull requests the publish packets opened, newest first — one per
/// packet that reached `open-pr`, with its reading and its state.
///
/// THE STATE IS WHAT GITHUB ANSWERED, never an inference (backlog
/// a5d4322c). It rides the packet as `pr_state`, written by the daily
/// `publish-github-pr.sh --measure` from GitHub's public pulls API, and
/// is taken only when its `pr_url` is this PR's. Until 2026-09-23 the
/// merge was inferred from a later mirror head equalling the PR's
/// snapshot commit — true only of a fast-forward, which GitHub's merge
/// never makes (#239 merged as merge commit b27382e5, #241 was squashed
/// to a7061022) — so every publish PR read open forever, and on
/// 2026-09-22 the region called #239 open for 86 hours, three days
/// after it merged.
pub fn publish_prs(packets: &[(Job, Vec<Step>)]) -> Vec<PublishPr> {
    let mut prs: Vec<PublishPr> = packets
        .iter()
        .filter_map(|(job, steps)| {
            let open_pr = find_step(steps, "open-pr", "open-pr")?;
            let opened = step_done_at(Some(open_pr))?;
            let url = md_str(&open_pr.metadata, "pr_url").to_string();
            if url.is_empty() {
                return None;
            }
            let snapshot = md_str(&open_pr.metadata, "snapshot_commit").to_string();
            let checks = find_step(steps, "read-checks", "read-checks");
            let judge = find_step(steps, "judge-checks", "judge-checks");
            let read = |key: &str| {
                checks
                    .map(|s| md_str(&s.metadata, key).to_string())
                    .unwrap_or_default()
            };
            let observed = job
                .metadata
                .get("pr_state")
                .filter(|o| md_str(o, "pr_url") == url);
            Some(PublishPr {
                packet_id: job.id.to_string(),
                merged: observed
                    .and_then(|o| o.get("merged"))
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                closed: observed.is_some_and(|o| md_str(o, "state") == "closed"),
                state_read_at: observed
                    .map(|o| md_str(o, "read_at").to_string())
                    .unwrap_or_default(),
                url,
                snapshot,
                opened,
                conclusion: read("conclusion"),
                alerts: read("alerts"),
                rules: read("rules"),
                failing: read("failing"),
                judged: judge.is_some_and(|s| s.status == StepStatus::Completed),
            })
        })
        .collect();
    prs.sort_by_key(|p| std::cmp::Reverse(p.opened));
    prs
}

/// THE HELD PACKET (backlog f49ae66d, the surface half of e1b6ddf7;
/// design cb38d806 §4). The oldest publish packet still OPEN that has
/// opened no pull request, held past [`STALLED_PUBLISH_HOURS`], with
/// what it has been held for, the step it waits at and the drift the
/// daily `--measure` wrote onto it. None when there is no such packet.
///
/// The PR conditions cannot see this case: a packet held before its
/// sign-off opens no PR, and while it is open the daily cadence spawns
/// nothing (`NOT open_publish_exists`), so a quiet region is exactly
/// what a hold used to look like — the 2026-09-17 -> 09-18 hold skipped
/// the week and #239 arrived as 1319 files. The drift reading on the
/// packet (`drift_refresh`, rewritten daily by
/// `publish-github-pr.sh --measure`) is what makes the sentence a
/// reading rather than an age.
fn held_publish(packets: &[(Job, Vec<Step>)], now: Instant) -> Option<(String, Instant)> {
    let (job, steps, since) = packets
        .iter()
        .filter(|(job, steps)| {
            job.status == JobStatus::Open
                && step_done_at(find_step(steps, "open-pr", "open-pr")).is_none()
        })
        .filter_map(|(job, steps)| Some((job, steps, job.opened_at.or_else(|| opened_at(job))?)))
        .filter(|(_, _, since)| (now - *since).num_hours() >= STALLED_PUBLISH_HOURS)
        .min_by_key(|(_, _, since)| *since)?;
    let waiting_at = steps
        .iter()
        .find(|s| matches!(s.status, StepStatus::Ready | StepStatus::Active))
        .map(|s| s.spec_slug.clone().unwrap_or_else(|| s.title.clone()))
        .unwrap_or_else(|| "no ready step".to_string());
    let drift = match job.metadata.get("drift_refresh") {
        Some(d) => format!(
            "the drift read {} commit(s) / {} file(s) ahead of the mirror at {}",
            md_str(d, "commits_ahead"),
            md_str(d, "files_changed"),
            md_str(d, "measured_at"),
        ),
        None => "no drift measurement is on the packet".to_string(),
    };
    Some((
        format!(
            "the publish packet opened {} has been held {} at {waiting_at} with no pull request — past the {STALLED_PUBLISH_HOURS}h a publish may stand, and no day behind it is published; {drift}",
            since.format("%Y-%m-%d"),
            plural(
                usize::try_from((now - since).num_hours()).unwrap_or(0),
                "hour",
                "hours"
            ),
        ),
        // The band is crossed the day after the packet opened.
        since + chrono::Duration::hours(STALLED_PUBLISH_HOURS),
    ))
}

/// THE PUBLISH REGION (design cb38d806, backlog eee42416) — the
/// crossing OUT of the world, where what landed on main is proposed to
/// the public mirror as a pull request and a person merges it.
///
/// The count is the pull requests the record shows STILL OPEN, because
/// that is the number the design's targets are written against: one
/// publish a day, sized like a day of trains (§1), and at most one
/// human act per publish (§3). A PR left standing is what turns the
/// next one into a week-scale snapshot no reader and no scan can judge.
///
/// The two troubled conditions are §4's, verbatim, and each carries
/// what it read rather than that something is wrong:
///
///   * A RED READING NOBODY JUDGED ([`PublishPr::unjudged_red`]) — the
///     defect §2 names. This leads, because it is the one a merge would
///     make permanent: #238 was merged over 64 unread alerts.
///   * A PULL REQUEST OPEN PAST [`STALLED_PUBLISH_HOURS`] — the publish
///     has stalled on the human gate, and every day it stands makes the
///     next diff larger.
///   * A PACKET HELD PAST [`STALLED_PUBLISH_HOURS`] WITH NO PULL REQUEST
///     ([`held_publish`], backlog f49ae66d) — the same stall one gate
///     earlier, at the sign-off, where no PR exists for the two above
///     to see. It reads last because it names a packet, not a PR.
///
/// The trend is publishes per day: the PRs opened in each window, which
/// is §1's own number.
pub(super) fn publish(inputs: &RegionInputs<'_>, w: &Windows) -> Region {
    const UNIT: &str = "pull requests awaiting merge";
    let Some(packets) = inputs.publish_packets else {
        return region(
            "publish",
            None,
            None,
            UNIT,
            unread_settled(
                "the publish-to-github packets could not be read",
                inputs.now,
            ),
            rate_trend("publishes", w, 0, 0),
            vec![measure(
                "mirror pull requests awaiting merge",
                None,
                "pull requests",
            )],
        );
    };
    let prs = publish_prs(packets);
    let (cur, prev) = count_split(w, prs.iter().map(|p| p.opened));
    let trend = rate_trend("publishes", w, cur, prev);

    let open: Vec<&PublishPr> = prs.iter().filter(|p| p.awaiting()).collect();
    let unjudged = open.iter().find(|p| p.unjudged_red());
    // Oldest first: the PR that has stood longest is the one the
    // sentence should name.
    let stalled = open
        .iter()
        .filter(|p| (inputs.now - p.opened).num_hours() >= STALLED_PUBLISH_HOURS)
        .min_by_key(|p| p.opened);
    let mut findings = Vec::new();
    if let Some(p) = unjudged {
        findings.push(Finding::new(
            bands::PUBLISH_UNJUDGED_RED,
            None,
            String::new(),
            format!("{} — {}", p.url, p.reading()),
        ));
    }
    if let Some(p) = stalled {
        let age = plural(
            usize::try_from((inputs.now - p.opened).num_hours()).unwrap_or(0),
            "hour",
            "hours",
        );
        // Openness is said only where GitHub was READ saying it
        // (backlog a5d4322c); an unread PR is named as unread.
        // And what failed on it, when a check did (backlog c6cb678b):
        // #243's only sentence was its age, with its Gate red.
        let failing = if p.failing.is_empty() {
            String::new()
        } else {
            format!(
                "; its checks read {} — failing: {}",
                p.conclusion, p.failing
            )
        };
        let why = if p.state_read_at.is_empty() {
            format!(
                "{} was opened {age} ago and its state was never read from GitHub — past the {STALLED_PUBLISH_HOURS}h a publish may stand, so it is stalled or unobserved{failing}",
                p.url
            )
        } else {
            format!(
                "{} has been open {age} (GitHub read it open at {}) — past the {STALLED_PUBLISH_HOURS}h a publish may stand, and every day it does the next diff is larger{failing}",
                p.url, p.state_read_at
            )
        };
        findings.push(Finding::new(
            bands::PUBLISH_PR_STALLED,
            Some(p.opened + chrono::Duration::hours(STALLED_PUBLISH_HOURS)),
            String::new(),
            why,
        ));
    }
    if let Some((why, since)) = held_publish(packets, inputs.now) {
        findings.push(Finding::new(
            bands::PUBLISH_HELD,
            Some(since),
            String::new(),
            why,
        ));
    }
    // A pull request inside its day is the publish WORKING — a person
    // merges it — and clear (decision 1).
    let clear_why = match open.first() {
        Some(p) => format!(
            "{} — {} awaiting a merge",
            p.url,
            plural(open.len(), "pull request", "pull requests")
        ),
        None => "no pull request awaiting a merge".to_string(),
    };
    let settled = settle(findings, clear_why, inputs.now);
    let kpi = vec![measure_said(
        "mirror pull requests awaiting merge",
        count_value(open.len()),
        "pull requests",
        format!(
            "{} awaiting merge",
            plural(open.len(), "mirror pull request", "mirror pull requests")
        ),
    )];
    region("publish", Some(open.len()), None, UNIT, settled, trend, kpi)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::regions::fixtures::*;

    /// One publish packet as the live ones are shaped (measured against
    /// b423d16b, 7d5c9051 and 254177e2 on 2026-09-22): the `open-pr`
    /// step carries the PR, its snapshot and the mirror head it was
    /// built on; `read-checks` carries the reading; `judge-checks` is
    /// skipped when the scan concluded `success`.
    fn publish_packet(
        pr: &str,
        snapshot: &str,
        mirror_head: &str,
        opened_at: &str,
        reading: Option<(&str, &str, &str)>,
        judged: bool,
    ) -> (Job, Vec<Step>) {
        let j = job(
            PUBLISH_KIND,
            "Publish to the public mirror",
            JobStatus::Closed,
            json!({}),
        );
        let mut open_pr = step(&j, "open-pr", StepStatus::Completed, Some(opened_at));
        open_pr.metadata = json!({
            "pr_url": pr,
            "snapshot_commit": snapshot,
            "mirror_head": mirror_head,
        });
        let mut steps = vec![open_pr];
        if let Some((conclusion, alerts, rules)) = reading {
            let mut checks = step(&j, "read-checks", StepStatus::Completed, Some(opened_at));
            checks.metadata = json!({ "conclusion": conclusion, "alerts": alerts, "rules": rules });
            steps.push(checks);
            steps.push(step(
                &j,
                "judge-checks",
                if judged {
                    StepStatus::Completed
                } else if conclusion == "success" {
                    StepStatus::Skipped
                } else {
                    StepStatus::Ready
                },
                judged.then_some(opened_at),
            ));
        }
        (j, steps)
    }

    fn with_publish<'a>(
        base: RegionInputs<'a>,
        packets: Option<&'a [(Job, Vec<Step>)]>,
    ) -> RegionInputs<'a> {
        RegionInputs {
            publish_packets: packets,
            ..base
        }
    }

    /// §4's first troubled condition: a pull request standing past a
    /// day. The record has the instant (`open-pr` completed) and what
    /// GitHub answered when `--measure` asked (`pr_state`), so the
    /// region can say STALLED from the record alone.
    #[test]
    fn a_mirror_pull_request_open_past_a_day_troubles_the_publish_region() {
        let status = empty_status();
        let base = inputs(&status, &[], &[], &[], &[], Some(&[]), Some(&[]));
        // Opened 30 hours before NOW, and GitHub, asked, said open.
        let packets = vec![observed(
            publish_packet(
                "https://mirror/pull/240",
                "snap-240",
                "mirror-a",
                "2026-09-18T06:00:00Z",
                Some(("success", "0", "0")),
                false,
            ),
            "https://mirror/pull/240",
            "open",
            false,
            "2026-09-19T00:01:08Z",
        )];
        let out = regions(&with_publish(base, Some(&packets)));
        let p = by_name(&out, "publish");
        assert_eq!(p.count, Some(1));
        assert!(
            p.why
                .contains("GitHub read it open at 2026-09-19T00:01:08Z"),
            "the why says when openness was observed: {}",
            p.why
        );
        assert_eq!(p.state, RegionState::Troubled, "{}", p.why);
        assert!(
            p.why.contains("pull/240"),
            "the why names the PR: {}",
            p.why
        );
        assert!(
            p.why.contains("30 hours"),
            "the why says how long it has stood: {}",
            p.why
        );
    }

    /// §4's second: a red reading with no disposition. The sentence
    /// carries THE READING — the conclusion and both counts the step
    /// recorded — not "something is wrong with the publish" (a verdict
    /// must name what failed).
    #[test]
    fn a_red_reading_nobody_judged_troubles_the_region_and_carries_the_reading() {
        let status = empty_status();
        let base = inputs(&status, &[], &[], &[], &[], Some(&[]), Some(&[]));
        // Opened an hour ago: not stalled, so the trouble can only be
        // the unjudged reading.
        let packets = vec![publish_packet(
            "https://mirror/pull/239",
            "snap-239",
            "mirror-a",
            "2026-09-19T11:00:00Z",
            Some(("failure", "109", "14")),
            false,
        )];
        let out = regions(&with_publish(base, Some(&packets)));
        let p = by_name(&out, "publish");
        assert_eq!(p.state, RegionState::Troubled, "{}", p.why);
        assert!(p.why.contains("pull/239"), "{}", p.why);
        assert!(
            p.why.contains("failure") && p.why.contains("109") && p.why.contains("14"),
            "the why is the reading, not a symptom: {}",
            p.why
        );

        // Judged, the same reading is no longer trouble: the merge now
        // follows a disposition per rule, which is all §2 asks.
        let judged = vec![publish_packet(
            "https://mirror/pull/239",
            "snap-239",
            "mirror-a",
            "2026-09-19T11:00:00Z",
            Some(("failure", "109", "14")),
            true,
        )];
        let base = inputs(&status, &[], &[], &[], &[], Some(&[]), Some(&[]));
        let out = regions(&with_publish(base, Some(&judged)));
        // A PR inside its day, judged, awaiting a person's merge: the
        // publish working (design 62de32ae, decision 1).
        assert_eq!(by_name(&out, "publish").state, RegionState::Clear);
        assert_eq!(
            by_name(&out, "publish").kpi[0].text,
            "1 mirror pull request awaiting merge"
        );
    }

    /// A FAILING CHECK IS NAMED (backlog c6cb678b). PR #243's Gate
    /// failed over a clean scan, and the region's only sentence about it
    /// — 25 hours later — was the PR's age. The reading now records
    /// `failing` on read-checks, and both troubled sentences carry it:
    /// the unjudged red, and the stalled PR after it was judged.
    #[test]
    fn a_failing_gate_is_named_in_the_publish_regions_why() {
        const GATE: &str = "Gate (infra/gate.sh, full): failure";
        let with_failing = |(j, mut steps): (Job, Vec<Step>)| {
            for s in &mut steps {
                if s.spec_slug.as_deref() == Some("read-checks") {
                    s.metadata["failing"] = json!(GATE);
                }
            }
            (j, steps)
        };
        let status = empty_status();

        // Unjudged, inside its day: the red reading leads, naming it.
        let base = inputs(&status, &[], &[], &[], &[], Some(&[]), Some(&[]));
        let packets = vec![with_failing(publish_packet(
            "https://mirror/pull/243",
            "snap-243",
            "mirror-a",
            "2026-09-19T11:00:00Z",
            Some(("failure", "3", "3")),
            false,
        ))];
        let out = regions(&with_publish(base, Some(&packets)));
        let p = by_name(&out, "publish");
        assert_eq!(p.state, RegionState::Troubled, "{}", p.why);
        assert!(
            p.why.contains(GATE),
            "the unjudged red names the failing check: {}",
            p.why
        );

        // Judged, and standing past its day: the age sentence names it too.
        let base = inputs(&status, &[], &[], &[], &[], Some(&[]), Some(&[]));
        let packets = vec![with_failing(publish_packet(
            "https://mirror/pull/243",
            "snap-243",
            "mirror-a",
            "2026-09-18T06:00:00Z",
            Some(("failure", "3", "3")),
            true,
        ))];
        let out = regions(&with_publish(base, Some(&packets)));
        let p = by_name(&out, "publish");
        assert_eq!(p.state, RegionState::Troubled, "{}", p.why);
        assert!(
            p.why.contains(GATE) && p.why.contains("hours"),
            "the stalled sentence names what failed beside the age: {}",
            p.why
        );
    }

    /// What `publish-github-pr.sh --measure` writes onto a publish
    /// packet when it asks GitHub about the pull request the packet
    /// opened (backlog a5d4322c): the PR it asked about, the state and
    /// merge GitHub answered, and when it asked.
    fn observed(
        (mut j, steps): (Job, Vec<Step>),
        pr: &str,
        state: &str,
        merged: bool,
        read_at: &str,
    ) -> (Job, Vec<Step>) {
        j.metadata = json!({ "pr_state": {
            "pr_url": pr, "state": state, "merged": merged, "read_at": read_at,
        }});
        (j, steps)
    }

    /// The merge is READ, never assumed. Measured 2026-09-22 (backlog
    /// a5d4322c): the region called #239 open for 86 hours and itself
    /// TROUBLED over it, while GitHub said #239 had merged three days
    /// earlier. It had inferred the merge from a later mirror head
    /// equalling the PR's snapshot, which a GitHub merge never makes
    /// true — #239 merged as merge commit b27382e5 and #241 was
    /// squashed to a7061022, neither of them the snapshot. So a later
    /// packet built on an old one's snapshot proves nothing; what
    /// GitHub answered does.
    #[test]
    fn a_pull_request_github_read_as_merged_leaves_the_count_and_clears_the_region() {
        let status = empty_status();
        let base = inputs(&status, &[], &[], &[], &[], Some(&[]), Some(&[]));
        let packets = vec![observed(
            publish_packet(
                "https://mirror/pull/239",
                "snap-239",
                "mirror-a",
                "2026-09-15T08:00:00Z",
                Some(("success", "0", "0")),
                false,
            ),
            "https://mirror/pull/239",
            "closed",
            true,
            "2026-09-19T00:01:08Z",
        )];
        let out = regions(&with_publish(base, Some(&packets)));
        let p = by_name(&out, "publish");
        assert_eq!(p.count, Some(0), "{}", p.why);
        assert_eq!(p.state, RegionState::Clear, "{}", p.why);

        // A PR closed WITHOUT a merge awaits nothing either.
        let base = inputs(&status, &[], &[], &[], &[], Some(&[]), Some(&[]));
        let packets = vec![observed(
            publish_packet(
                "https://mirror/pull/238",
                "snap-238",
                "mirror-a",
                "2026-09-15T08:00:00Z",
                Some(("success", "0", "0")),
                false,
            ),
            "https://mirror/pull/238",
            "closed",
            false,
            "2026-09-19T00:01:08Z",
        )];
        let out = regions(&with_publish(base, Some(&packets)));
        assert_eq!(by_name(&out, "publish").count, Some(0));

        // The retired inference: a later snapshot built on this one's is
        // NOT a merge the region may claim.
        let base = inputs(&status, &[], &[], &[], &[], Some(&[]), Some(&[]));
        let packets = vec![
            publish_packet(
                "https://mirror/pull/240",
                "snap-240",
                "mirror-a",
                "2026-09-19T11:00:00Z",
                Some(("success", "0", "0")),
                false,
            ),
            publish_packet(
                "https://mirror/pull/241",
                "snap-241",
                "snap-240",
                "2026-09-19T11:30:00Z",
                Some(("success", "0", "0")),
                false,
            ),
        ];
        let out = regions(&with_publish(base, Some(&packets)));
        assert_eq!(
            by_name(&out, "publish").count,
            Some(2),
            "a mirror head is not GitHub's answer"
        );
    }

    /// The sentence says only what was observed. A PR whose state was
    /// never read from GitHub is NOT "open for N hours" — that is the
    /// claim the region could not earn (backlog a5d4322c) — it is a PR
    /// nobody asked about, and it still troubles the region past a day,
    /// because an unasked question is not a pass.
    #[test]
    fn a_pull_request_never_read_from_github_is_not_called_open() {
        let status = empty_status();
        let base = inputs(&status, &[], &[], &[], &[], Some(&[]), Some(&[]));
        let packets = vec![publish_packet(
            "https://mirror/pull/239",
            "snap-239",
            "mirror-a",
            "2026-09-18T06:00:00Z",
            Some(("success", "0", "0")),
            false,
        )];
        let out = regions(&with_publish(base, Some(&packets)));
        let p = by_name(&out, "publish");
        assert_eq!(p.state, RegionState::Troubled, "{}", p.why);
        assert!(!p.why.contains("has been open"), "{}", p.why);
        assert!(p.why.contains("never read from GitHub"), "{}", p.why);

        // A reading of ANOTHER pull request is no reading of this one.
        let base = inputs(&status, &[], &[], &[], &[], Some(&[]), Some(&[]));
        let packets = vec![observed(
            publish_packet(
                "https://mirror/pull/239",
                "snap-239",
                "mirror-a",
                "2026-09-18T06:00:00Z",
                Some(("success", "0", "0")),
                false,
            ),
            "https://mirror/pull/238",
            "closed",
            true,
            "2026-09-19T00:01:08Z",
        )];
        let out = regions(&with_publish(base, Some(&packets)));
        let p = by_name(&out, "publish");
        assert_eq!(p.count, Some(1), "{}", p.why);
        assert!(p.why.contains("never read from GitHub"), "{}", p.why);
    }

    /// A publish packet HELD before its pull request, as the live one
    /// was shaped on 2026-09-23 (d2967a9c): open, `open-pr` not reached,
    /// the step it waits at `ready`, and the daily `--measure` reading
    /// on its metadata as `drift_refresh`.
    fn held_packet(opened_at: &str, waiting_at: &str, drift: Option<Value>) -> (Job, Vec<Step>) {
        let mut j = job(
            PUBLISH_KIND,
            "Publish to the public mirror",
            JobStatus::Open,
            drift
                .map(
                    |d| json!({ "drift_refresh": d, "drift_refreshed_at": "2026-09-19T00:01:08Z" }),
                )
                .unwrap_or_else(|| json!({})),
        );
        j.opened_at = Some(t(opened_at));
        let steps = vec![
            step(&j, "opened", StepStatus::Completed, Some(opened_at)),
            step(&j, waiting_at, StepStatus::Ready, None),
            step(&j, "open-pr", StepStatus::Pending, None),
        ];
        (j, steps)
    }

    /// THE HELD PACKET (backlog f49ae66d, the surface half of e1b6ddf7;
    /// design cb38d806). A packet left open before its sign-off opens no
    /// pull request, so the two PR conditions cannot see it — and it is
    /// the hold that cost the week of 2026-09-17 -> 09-18, because the
    /// daily cadence spawns nothing while a packet is open. Held past a
    /// day it is troubled, and the sentence carries the step it waits
    /// at and the drift `--measure` wrote onto it, not a symptom.
    #[test]
    fn a_publish_packet_held_past_a_day_without_a_pull_request_troubles_the_region() {
        let status = empty_status();
        let base = inputs(&status, &[], &[], &[], &[], Some(&[]), Some(&[]));
        let packets = vec![held_packet(
            "2026-09-17T12:00:00Z",
            "approve",
            Some(json!({
                "commits_ahead": "495",
                "files_changed": "114",
                "has_drift": "true",
                "measured_at": "2026-09-19T00:01:08Z",
            })),
        )];
        let out = regions(&with_publish(base, Some(&packets)));
        let p = by_name(&out, "publish");
        assert_eq!(p.state, RegionState::Troubled, "{}", p.why);
        assert_eq!(
            p.count,
            Some(0),
            "the count is still pull requests: {}",
            p.why
        );
        assert!(
            p.why.contains("48 hours"),
            "how long it has been held: {}",
            p.why
        );
        assert!(p.why.contains("approve"), "the step it waits at: {}", p.why);
        assert!(
            p.why.contains("495")
                && p.why.contains("114")
                && p.why.contains("2026-09-19T00:01:08Z"),
            "the dated drift reading: {}",
            p.why
        );

        // No reading on it yet is said, never drawn as a current mirror.
        let base = inputs(&status, &[], &[], &[], &[], Some(&[]), Some(&[]));
        let bare = vec![held_packet("2026-09-17T12:00:00Z", "measure", None)];
        let out = regions(&with_publish(base, Some(&bare)));
        let p = by_name(&out, "publish");
        assert_eq!(p.state, RegionState::Troubled, "{}", p.why);
        assert!(p.why.contains("no drift measurement"), "{}", p.why);
    }

    /// Today's packet, a few hours into its own measure step, is the
    /// cadence working — not a hold. And a packet that CLOSED without a
    /// pull request (`nothing-to-publish`, `declined`) holds nothing.
    #[test]
    fn a_publish_packet_inside_its_day_or_closed_does_not_trouble_the_region() {
        let status = empty_status();
        let base = inputs(&status, &[], &[], &[], &[], Some(&[]), Some(&[]));
        let fresh = vec![held_packet("2026-09-19T08:00:00Z", "measure", None)];
        let out = regions(&with_publish(base, Some(&fresh)));
        assert_eq!(by_name(&out, "publish").state, RegionState::Clear);

        let (mut closed, steps) = held_packet("2026-09-15T00:00:00Z", "declined", None);
        closed.status = JobStatus::Closed;
        let base = inputs(&status, &[], &[], &[], &[], Some(&[]), Some(&[]));
        let done = vec![(closed, steps)];
        let out = regions(&with_publish(base, Some(&done)));
        assert_eq!(by_name(&out, "publish").state, RegionState::Clear);
    }

    /// An unread publish list is troubled, never a mirror that reads as
    /// current — the rule every region here keeps.
    #[test]
    fn an_unread_publish_list_is_troubled_and_never_a_current_mirror() {
        let status = empty_status();
        let base = inputs(&status, &[], &[], &[], &[], Some(&[]), Some(&[]));
        let out = regions(&with_publish(base, None));
        let p = by_name(&out, "publish");
        assert_eq!(p.count, None);
        assert_eq!(p.state, RegionState::Troubled);
        assert!(p.why.contains("could not be read"), "{}", p.why);
    }
}
