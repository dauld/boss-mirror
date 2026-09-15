//! EACH SIDING LANDS ON ITS OWN EVIDENCE — design c6bd173e, car 3
//! (backlog edae6e8b, 2026-09-15).
//!
//! Car 1 laid the four arrivals sidings and placed a landed wagon on
//! the siding of its `delivery_channel`; car 2 stamped a train with the
//! heaviest of its cars'. Both still read "landed" off ONE fact — the
//! train's `converged` step, the image roll — whatever the car changed.
//! The design's first decision says otherwise: a wagon lands on its
//! siding when ITS channel's live evidence exists. This module is that
//! reading, per car, from the packets each channel's converge already
//! leaves behind.
//!
//! WHAT EACH CHANNEL'S EVIDENCE IS — measured 2026-09-15 on the system
//! of record for train #382 (merge 34db709 at 16:10:28Z):
//!
//! - **config** — the `maintenance-cluster-converge` packet: the
//!   forge's deploy runner builds forge main, applies
//!   infra/cluster/manifests and verifies them, and its `run` step
//!   carries `build_head` (a SHORT sha) with `verify_s` when the
//!   manifests verified, or `unchanged` when the head was already on.
//!   Closed 16:14:46Z — six minutes BEFORE the converged step.
//! - **software** — the train's `converged` step: the conductor read
//!   the cluster jobs API self-reporting the merge (`cluster_commit`),
//!   16:20:45Z.
//! - **infra** — the host converge packets, `maintenance-forge-converge`
//!   (forge, 16:16:07Z) and `maintenance-boss-gcp-converge` (boss-gcp,
//!   16:34:23Z — fourteen minutes AFTER the image roll, which is the
//!   case car 1 could not draw). Each `run` step carries the
//!   `converge_sha` the host is on and its `node_id`.
//! - **data** — nothing of its own yet. The design names the
//!   live-protocols / live-rules equality read off the SoR; today that
//!   equality is a tree-side lint over the registry's kind SET, and the
//!   registry loads its files at the boot of the converged image. So a
//!   data car lands on the software evidence, and its row SAYS so.
//!
//! WHAT THIS READER CANNOT KNOW, AND HOW IT COPES. The yard has no git,
//! so it cannot ask whether a later head CONTAINS a merge. The
//! conductor can, and already did: a `converged` step's
//! `cluster_commit` is a head the conductor proved contains the merge
//! (by equality or `merge-base --is-ancestor`, train.rs). So a converge
//! packet at EITHER the merge sha or that cluster commit is evidence
//! for the merge. A host that skipped both — moved from before the
//! merge straight past the cluster's commit — reads as still converging
//! until the window says it cannot judge; the honest gap, named on the
//! row rather than papered over.
//!
//! The packets are read in a WINDOW, and a window is not the record.
//! A train whose merge predates the oldest packet read cannot be judged
//! "converging" — the evidence may simply be older than the read — so
//! its landing is `unread`, with the window's edge on the row.

use boss_core::job::{Job, JobStatus, Step, StepStatus};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The packet kind the cluster converge leaves (cluster-deploy-runner
/// on the forge host, infra/platform/workflows/maintenance-cluster-converge.toml).
pub const CLUSTER_CONVERGE: &str = "maintenance-cluster-converge";
/// The packet kinds the host converges leave, one per host that
/// converges itself onto forge main.
pub const HOST_CONVERGES: [&str; 2] = [
    "maintenance-forge-converge",
    "maintenance-boss-gcp-converge",
];
/// How many packets of each converge kind the handler reads. Thirty is
/// five hours of the ten-minute cluster timer and fifteen of the
/// half-hour boss-gcp one; `unchanged` ticks count as evidence too, so
/// a head the cluster is still on stays judgeable long after its build
/// packet left the window. Past the window the row says `unread`.
pub const CONVERGE_WINDOW: i64 = 30;

/// The four sidings, in the order the yard lays them.
const CHANNELS: [&str; 4] = ["data", "config", "software", "infra"];

/// Whether a car's channel evidence exists — the reading a wagon on an
/// arrivals siding is drawn from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Landing {
    /// The channel's live evidence exists: which packet or step, and
    /// when it was recorded.
    Landed {
        evidence: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        at: Option<String>,
    },
    /// Not yet — the evidence it waits for, named, so the operator
    /// knows what to look at rather than how long to wait.
    Converging { awaiting: String },
    /// The read did not reach the packet that would decide it: the
    /// window of converge packets begins after this merge.
    Unread { why: String },
}

/// One car on its siding, judged on its own channel's evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SidingCar {
    /// The car's packet id.
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// The train that carried it.
    pub train: String,
    /// The siding: the car's `delivery_channel`, `software` when the
    /// packet carries none (the car's own default, `yard.ts`).
    pub channel: String,
    pub landing: Landing,
}

fn meta_str<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str)
}

/// A step by its slug, falling back to its title — the conductor's own
/// addressing, so the two ends agree.
fn step<'a>(steps: &'a [Step], slug: &str, title: &str) -> Option<&'a Step> {
    steps
        .iter()
        .find(|s| s.spec_slug.as_deref() == Some(slug) || s.title == title)
}

/// Two commit refs name one commit when either is a prefix of the other
/// — the conductor's `commits_match`: the runner tags images with the
/// short sha and attests the full one.
fn commits_match(a: &str, b: &str) -> bool {
    !a.is_empty() && !b.is_empty() && (a.starts_with(b) || b.starts_with(a))
}

/// The first `n` bytes of a stamp — whole, when the stamp is shorter or
/// a char boundary refuses the cut (a malformed stamp must not panic a
/// read-model).
fn prefix(s: &str, n: usize) -> &str {
    s.get(..n).unwrap_or(s)
}

fn short(sha: &str) -> &str {
    prefix(sha, 7)
}

fn id8(id: &str) -> &str {
    prefix(id, 8)
}

fn parse_instant(s: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|t| t.with_timezone(&chrono::Utc))
}

/// When a packet opened: `metadata.opened_at` (the maintenance wrap's
/// stamp), else the `opened_on` date at midnight.
fn opened_at(job: &Job) -> Option<chrono::DateTime<chrono::Utc>> {
    meta_str(&job.metadata, "opened_at")
        .and_then(parse_instant)
        .or_else(|| job.opened_on.and_hms_opt(0, 0, 0).map(|t| t.and_utc()))
}

fn closed_at(job: &Job) -> Option<String> {
    meta_str(&job.metadata, "closed_at").map(str::to_string)
}

/// A closed packet whose `run` step ended `ok` — the workflow's own
/// `completed` terminal predicate.
fn run_ok(job: &Job, steps: &[Step]) -> bool {
    job.status == JobStatus::Closed
        && step(steps, "run", "run").is_some_and(|s| meta_str(&s.metadata, "result") == Some("ok"))
}

fn run_meta(steps: &[Step]) -> Option<&Value> {
    step(steps, "run", "run").map(|s| &s.metadata)
}

/// What a train proved about its own merge, read once per train.
struct MergedTrain<'a> {
    id: String,
    merge_ref: &'a str,
    merged_at: Option<chrono::DateTime<chrono::Utc>>,
    /// The `converged` step's `cluster_commit` and stamp, once done.
    converged: Option<(&'a str, Option<&'a str>)>,
}

impl MergedTrain<'_> {
    /// The heads that carry this merge: the merge itself, and the head
    /// the conductor proved contains it.
    fn heads(&self) -> Vec<&str> {
        let mut out = vec![self.merge_ref];
        if let Some((cc, _)) = self.converged
            && !commits_match(cc, self.merge_ref)
        {
            out.push(cc);
        }
        out
    }

    fn any_head_matches(&self, sha: Option<&str>) -> bool {
        sha.is_some_and(|s| self.heads().iter().any(|h| commits_match(h, s)))
    }
}

fn merged_train<'a>(job: &'a Job, steps: &'a [Step]) -> Option<MergedTrain<'a>> {
    let merged =
        step(steps, "merged", "Merged into main").filter(|s| s.status == StepStatus::Completed)?;
    let merge_ref = meta_str(&merged.metadata, "merge_ref")?;
    let converged = step(steps, "converged", "Cluster converged")
        .filter(|s| s.status == StepStatus::Completed)
        .and_then(|s| {
            meta_str(&s.metadata, "cluster_commit")
                .map(|cc| (cc, meta_str(&s.metadata, "completed_at")))
        });
    Some(MergedTrain {
        id: job.id.to_string(),
        merge_ref,
        merged_at: meta_str(&merged.metadata, "completed_at").and_then(parse_instant),
        converged,
    })
}

/// Does a window of packets reach back to the merge? The oldest packet
/// read opened no later than the merge — so every packet since the
/// merge was read, and "none matched" is a reading. No merge stamp is
/// read as reaching: the row then says converging rather than hiding
/// behind a missing clock.
fn window_reaches(
    oldest: Option<chrono::DateTime<chrono::Utc>>,
    merged_at: Option<chrono::DateTime<chrono::Utc>>,
) -> bool {
    match (oldest, merged_at) {
        (Some(o), Some(m)) => o <= m,
        (None, _) => false,
        (_, None) => true,
    }
}

fn oldest_opened<'a>(
    packets: impl Iterator<Item = &'a Job>,
) -> Option<chrono::DateTime<chrono::Utc>> {
    packets.filter_map(opened_at).min()
}

/// The software reading: the train's `converged` step, as today.
fn software_landing(t: &MergedTrain<'_>) -> Landing {
    match t.converged {
        Some((cc, at)) => Landing::Landed {
            evidence: format!(
                "the cluster jobs API self-reports {} — the train's converged step",
                short(cc)
            ),
            at: at.map(str::to_string),
        },
        None => Landing::Converging {
            awaiting: format!(
                "the train's converged step — the cluster jobs API self-reporting {}",
                short(t.merge_ref)
            ),
        },
    }
}

/// The data reading: the software evidence, and the row says why —
/// the registry loads its files at the boot of the converged image,
/// and no per-file equality is on the system of record yet.
fn data_landing(t: &MergedTrain<'_>) -> Landing {
    match t.converged {
        Some((cc, at)) => Landing::Landed {
            evidence: format!(
                "the registry loaded its files at the boot of {} — the train's converged step (no per-file reading exists yet)",
                short(cc)
            ),
            at: at.map(str::to_string),
        },
        None => Landing::Converging {
            awaiting: format!(
                "the train's converged step — the registry reloads its files when the image at {} boots",
                short(t.merge_ref)
            ),
        },
    }
}

/// The config reading: a cluster converge that built (and verified the
/// manifests at) a head carrying the merge, or reports main unchanged
/// at one — the stamp `unchanged` reads is written only after a roll.
fn config_landing(t: &MergedTrain<'_>, converges: &[(Job, Vec<Step>)]) -> Landing {
    let cluster: Vec<&(Job, Vec<Step>)> = converges
        .iter()
        .filter(|(j, _)| j.kind == CLUSTER_CONVERGE)
        .collect();
    let built = cluster.iter().find(|(j, s)| {
        run_ok(j, s) && t.any_head_matches(run_meta(s).and_then(|m| meta_str(m, "build_head")))
    });
    if let Some((j, s)) = built {
        let head = run_meta(s)
            .and_then(|m| meta_str(m, "build_head"))
            .unwrap_or("");
        return Landing::Landed {
            evidence: format!(
                "manifests applied and verified at {head} — {CLUSTER_CONVERGE} {}",
                id8(&j.id.to_string())
            ),
            at: closed_at(j),
        };
    }
    let unchanged = cluster.iter().find(|(j, s)| {
        run_ok(j, s) && t.any_head_matches(run_meta(s).and_then(|m| meta_str(m, "unchanged")))
    });
    if let Some((j, s)) = unchanged {
        let head = run_meta(s)
            .and_then(|m| meta_str(m, "unchanged"))
            .unwrap_or("");
        return Landing::Landed {
            evidence: format!(
                "the cluster converge reports main unchanged at {head} — manifests already applied ({CLUSTER_CONVERGE} {})",
                id8(&j.id.to_string())
            ),
            at: closed_at(j),
        };
    }
    // A run that built the head and died says so: the next tick
    // retries, and the operator should read that run, not wait.
    let died = cluster.iter().find(|(j, s)| {
        j.status == JobStatus::Closed
            && t.any_head_matches(run_meta(s).and_then(|m| meta_str(m, "build_head")))
    });
    if let Some((j, s)) = died {
        let result = run_meta(s)
            .and_then(|m| meta_str(m, "result"))
            .unwrap_or("no result");
        return Landing::Converging {
            awaiting: format!(
                "the converge that built {} ended {result} ({CLUSTER_CONVERGE} {}) — the next tick retries",
                short(t.merge_ref),
                id8(&j.id.to_string())
            ),
        };
    }
    let oldest = oldest_opened(cluster.iter().map(|(j, _)| j));
    if window_reaches(oldest, t.merged_at) {
        Landing::Converging {
            awaiting: format!(
                "a {CLUSTER_CONVERGE} packet that built {} and verified the manifests",
                short(t.merge_ref)
            ),
        }
    } else {
        Landing::Unread {
            why: unread_why(CLUSTER_CONVERGE, oldest),
        }
    }
}

fn unread_why(kind: &str, oldest: Option<chrono::DateTime<chrono::Utc>>) -> String {
    match oldest {
        Some(o) => format!(
            "the {CONVERGE_WINDOW} newest {kind} packets begin at {} — after this merge",
            o.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
        ),
        None => format!("no {kind} packet in the window"),
    }
}

/// The host a host-converge packet speaks for: its `node_id`, else the
/// host its kind names (a packet from before `node_id` was recorded).
fn host_of(job: &Job, steps: &[Step]) -> String {
    run_meta(steps)
        .and_then(|m| meta_str(m, "node_id"))
        .filter(|h| !h.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| {
            job.kind
                .trim_start_matches("maintenance-")
                .trim_end_matches("-converge")
                .to_string()
        })
}

/// The infra reading: every host that converges itself has reported a
/// sha carrying the merge. A host is a name the window shows — a host
/// with no packet at all is the estate observer's finding, not this
/// row's — and the hosts are named in the evidence and in what is
/// awaited, since "the host converged" is a fact per host.
fn infra_landing(t: &MergedTrain<'_>, converges: &[(Job, Vec<Step>)]) -> Landing {
    let host_runs: Vec<&(Job, Vec<Step>)> = converges
        .iter()
        .filter(|(j, _)| HOST_CONVERGES.contains(&j.kind.as_str()))
        .collect();
    let mut hosts: Vec<String> = host_runs.iter().map(|(j, s)| host_of(j, s)).collect();
    hosts.sort();
    hosts.dedup();
    if hosts.is_empty() {
        return Landing::Unread {
            why: "no host converge packet in the window".to_string(),
        };
    }
    let mut landed: Vec<(String, String, Option<String>)> = Vec::new();
    let mut waiting: Vec<String> = Vec::new();
    let mut unread: Vec<String> = Vec::new();
    for host in &hosts {
        let runs = host_runs.iter().filter(|(j, s)| host_of(j, s) == *host);
        let hit = runs.clone().find(|(j, s)| {
            run_ok(j, s)
                && run_meta(s).is_some_and(|m| {
                    t.any_head_matches(meta_str(m, "converge_sha"))
                        || t.any_head_matches(meta_str(m, "converge_from"))
                })
        });
        match hit {
            Some((j, _)) => landed.push((
                host.clone(),
                id8(&j.id.to_string()).to_string(),
                closed_at(j),
            )),
            None => {
                let oldest = oldest_opened(runs.clone().map(|(j, _)| j));
                if window_reaches(oldest, t.merged_at) {
                    let last = runs
                        .clone()
                        .find(|(j, s)| run_ok(j, s))
                        .and_then(|(_, s)| run_meta(s).and_then(|m| meta_str(m, "converge_sha")))
                        .map(|sha| format!(" — last reported {}", short(sha)))
                        .unwrap_or_default();
                    waiting.push(format!("{host}{last}"));
                } else {
                    unread.push(host.clone());
                }
            }
        }
    }
    if !waiting.is_empty() {
        return Landing::Converging {
            awaiting: format!(
                "host converge on {}: {}",
                short(t.merge_ref),
                waiting.join(", ")
            ),
        };
    }
    if !unread.is_empty() {
        return Landing::Unread {
            why: format!(
                "the {CONVERGE_WINDOW} newest host converge packets for {} begin after this merge",
                unread.join(", ")
            ),
        };
    }
    let at = landed.iter().filter_map(|(_, _, at)| at.clone()).max();
    Landing::Landed {
        evidence: format!(
            "{} converged on {} ({})",
            landed
                .iter()
                .map(|(h, _, _)| h.as_str())
                .collect::<Vec<_>>()
                .join(", "),
            short(t.merge_ref),
            landed
                .iter()
                .map(|(_, id, _)| id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
        at,
    }
}

/// The siding a car stands on: its stamp when it names one of the four,
/// else software — the car's own default (`yard.ts::deliveryChannelOf`).
fn channel_of(car: &Job) -> &str {
    meta_str(&car.metadata, "delivery_channel")
        .filter(|c| CHANNELS.contains(c))
        .unwrap_or("software")
}

/// The sidings lane: one row per car of every MERGED train given, in
/// the order the trains come, judged on the car's channel evidence.
/// A train not yet merged has nothing live to judge and contributes no
/// row; a boarded car outside `cars` (the handler's window) is skipped
/// rather than guessed a channel.
pub fn sidings<'a>(
    trains: impl IntoIterator<Item = &'a (Job, Vec<Step>)>,
    cars: &[Job],
    converges: &[(Job, Vec<Step>)],
) -> Vec<SidingCar> {
    trains
        .into_iter()
        .filter_map(|(j, s)| merged_train(j, s).map(|t| (j, t)))
        .flat_map(|(j, t)| {
            let boarded: Vec<&Job> = j
                .metadata
                .get("boarded_jobs")
                .and_then(Value::as_array)
                .map(|ids| {
                    ids.iter()
                        .filter_map(Value::as_str)
                        .filter_map(|id| cars.iter().find(|c| c.id.to_string() == id))
                        .collect()
                })
                .unwrap_or_default();
            boarded
                .into_iter()
                .map(|car| {
                    let channel = channel_of(car);
                    let landing = match channel {
                        "config" => config_landing(&t, converges),
                        "infra" => infra_landing(&t, converges),
                        "data" => data_landing(&t),
                        _ => software_landing(&t),
                    };
                    SidingCar {
                        id: car.id.to_string(),
                        branch: meta_str(&car.metadata, "branch").map(str::to_string),
                        train: t.id.clone(),
                        channel: channel.to_string(),
                        landing,
                    }
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use boss_core::job::{JobId, Priority, Subject};
    use serde_json::json;

    fn job(kind: &str, status: JobStatus, metadata: Value) -> Job {
        Job {
            id: JobId::new(),
            kind: kind.into(),
            workflow_version: 1,
            subject: Subject::new("custom", "x"),
            title: kind.into(),
            owner_id: "emp-david".into(),
            status,
            priority: Priority::Standard,
            opened_on: chrono::NaiveDate::from_ymd_opt(2026, 9, 15).unwrap(),
            due_on: None,
            closed_on: None,
            metadata,
            tags: vec![],
            simulated: false,
        }
    }

    fn step_done(slug: &str, title: &str, metadata: Value) -> Step {
        let mut s = Step::new(Default::default(), "task", title, 0);
        s.spec_slug = Some(slug.into());
        s.status = StepStatus::Completed;
        s.metadata = metadata;
        s
    }

    fn step_ready(slug: &str, title: &str) -> Step {
        let mut s = Step::new(Default::default(), "task", title, 0);
        s.spec_slug = Some(slug.into());
        s.status = StepStatus::Ready;
        s
    }

    const MERGE: &str = "34db7093e3b7";
    const MERGED_AT: &str = "2026-09-15T16:10:28Z";

    fn car(channel: Option<&str>) -> Job {
        let mut md = json!({ "branch": "fix/x" });
        if let Some(c) = channel {
            md["delivery_channel"] = json!(c);
        }
        job("ship-a-change", JobStatus::Open, md)
    }

    /// A merged train carrying `cars`, converged (software evidence on
    /// the record) or not.
    fn train(cars: &[&Job], converged: bool) -> (Job, Vec<Step>) {
        let ids: Vec<String> = cars.iter().map(|c| c.id.to_string()).collect();
        let j = job("pr-train", JobStatus::Open, json!({ "boarded_jobs": ids }));
        let mut steps = vec![step_done(
            "merged",
            "Merged into main",
            json!({ "merge_ref": MERGE, "completed_at": MERGED_AT }),
        )];
        steps.push(if converged {
            step_done(
                "converged",
                "Cluster converged",
                json!({
                    "cluster_commit": "34db7093e3b7f9394ff469b92a301fb39a731a80",
                    "completed_at": "2026-09-15T16:20:45Z"
                }),
            )
        } else {
            step_ready("converged", "Cluster converged")
        });
        (j, steps)
    }

    /// A converge packet of `kind` that ended `ok`, with the run step's
    /// summary fields as the run wrote them (strings).
    fn converge(kind: &str, opened: &str, closed: &str, run: Value) -> (Job, Vec<Step>) {
        let mut run = run;
        run["result"] = json!("ok");
        let j = job(
            kind,
            JobStatus::Closed,
            json!({ "opened_at": opened, "closed_at": closed, "outcome": "completed" }),
        );
        (j, vec![step_done("run", "run", run)])
    }

    fn landing_of(rows: &[SidingCar], car: &Job) -> Landing {
        rows.iter()
            .find(|r| r.id == car.id.to_string())
            .unwrap_or_else(|| panic!("no siding row for {}", car.id))
            .landing
            .clone()
    }

    fn evidence(l: &Landing) -> &str {
        match l {
            Landing::Landed { evidence, .. } => evidence,
            other => panic!("expected landed, got {other:?}"),
        }
    }

    fn awaiting(l: &Landing) -> &str {
        match l {
            Landing::Converging { awaiting } => awaiting,
            other => panic!("expected converging, got {other:?}"),
        }
    }

    // ---- software: as today, the train's converged step ----

    #[test]
    fn a_software_car_lands_on_the_converged_step_and_waits_on_it_before() {
        let c = car(Some("software"));
        let rows = sidings(&[train(&[&c], true)], std::slice::from_ref(&c), &[]);
        let l = landing_of(&rows, &c);
        assert!(evidence(&l).contains("converged step"), "{l:?}");
        assert!(evidence(&l).contains("34db709"), "{l:?}");
        assert_eq!(rows[0].channel, "software");

        let rows = sidings(&[train(&[&c], false)], std::slice::from_ref(&c), &[]);
        assert!(awaiting(&landing_of(&rows, &c)).contains("converged step"));
    }

    /// An old car — parked before the stamp existed — is software, the
    /// car's own default, never guessed lighter.
    #[test]
    fn a_car_with_no_stamp_stands_on_the_software_siding() {
        let c = car(None);
        let rows = sidings(&[train(&[&c], true)], std::slice::from_ref(&c), &[]);
        assert_eq!(rows[0].channel, "software");
    }

    /// A data car has no evidence of its own yet and its row says so —
    /// it lands on the software fact, named as such, not on a claim
    /// about its files.
    #[test]
    fn a_data_car_lands_on_the_software_evidence_and_says_so() {
        let c = car(Some("data"));
        let rows = sidings(&[train(&[&c], true)], std::slice::from_ref(&c), &[]);
        let l = landing_of(&rows, &c);
        assert_eq!(rows[0].channel, "data");
        assert!(evidence(&l).contains("no per-file reading"), "{l:?}");
    }

    // ---- config: the cluster converge packet ----

    /// The measured case: the converge that built the merge closed six
    /// minutes BEFORE the conductor stamped `converged`. A config car
    /// lands on the packet, whether or not the train has converged.
    #[test]
    fn a_config_car_lands_when_the_cluster_converge_built_and_verified_its_merge() {
        let c = car(Some("config"));
        let built = converge(
            CLUSTER_CONVERGE,
            "2026-09-15T16:11:13Z",
            "2026-09-15T16:14:46Z",
            json!({ "build_head": "34db709", "build_s": "165", "roll_s": "23", "verify_s": "6" }),
        );
        let rows = sidings(
            &[train(&[&c], false)],
            std::slice::from_ref(&c),
            std::slice::from_ref(&built),
        );
        let l = landing_of(&rows, &c);
        assert!(
            evidence(&l).contains("manifests applied and verified at 34db709"),
            "{l:?}"
        );
        assert!(evidence(&l).contains(id8(&built.0.id.to_string())), "{l:?}");
        assert_eq!(
            match &l {
                Landing::Landed { at, .. } => at.as_deref(),
                _ => None,
            },
            Some("2026-09-15T16:14:46Z")
        );
    }

    /// The stamp `unchanged` reads is written after the roll, so a tick
    /// reporting main unchanged at the merge is evidence too — and it
    /// keeps a head judgeable after its build packet left the window.
    #[test]
    fn a_config_car_lands_on_an_unchanged_tick_at_its_merge() {
        let c = car(Some("config"));
        let tick = converge(
            CLUSTER_CONVERGE,
            "2026-09-15T20:25:53Z",
            "2026-09-15T20:25:54Z",
            json!({ "unchanged": "34db709" }),
        );
        let rows = sidings(&[train(&[&c], false)], std::slice::from_ref(&c), &[tick]);
        assert!(evidence(&landing_of(&rows, &c)).contains("unchanged at 34db709"));
    }

    /// The cluster rolled PAST the merge: the conductor proved the later
    /// head contains it (`cluster_commit`), so a converge at that head
    /// is evidence for this merge — the yard borrows the ancestry it
    /// cannot compute.
    #[test]
    fn a_config_car_lands_on_a_converge_at_the_head_the_conductor_proved_contains_it() {
        let c = car(Some("config"));
        let (j, mut s) = train(&[&c], true);
        s.iter_mut()
            .find(|st| st.spec_slug.as_deref() == Some("converged"))
            .unwrap()
            .metadata["cluster_commit"] = json!("6c13191bbda28805976716d00bceb12a55aef0c7");
        let later = converge(
            CLUSTER_CONVERGE,
            "2026-09-15T16:30:00Z",
            "2026-09-15T16:34:00Z",
            json!({ "build_head": "6c13191", "verify_s": "6" }),
        );
        let rows = sidings(&[(j, s)], std::slice::from_ref(&c), &[later]);
        assert!(evidence(&landing_of(&rows, &c)).contains("6c13191"));
    }

    /// No packet yet, but the window reaches back past the merge: the
    /// car is CONVERGING on its siding, with the evidence it waits for
    /// named — not landed because its train arrived.
    #[test]
    fn a_config_car_whose_manifests_have_not_applied_stands_converging_even_after_the_train_arrived()
     {
        let c = car(Some("config"));
        let before = converge(
            CLUSTER_CONVERGE,
            "2026-09-15T16:01:00Z",
            "2026-09-15T16:01:01Z",
            json!({ "unchanged": "1111111" }),
        );
        let rows = sidings(&[train(&[&c], true)], std::slice::from_ref(&c), &[before]);
        let l = landing_of(&rows, &c);
        assert!(awaiting(&l).contains(CLUSTER_CONVERGE), "{l:?}");
        assert!(awaiting(&l).contains("34db709"), "{l:?}");
    }

    /// A run that built the head and died is named, not waited on.
    #[test]
    fn a_converge_that_died_on_the_merge_is_named_on_the_row() {
        let c = car(Some("config"));
        let (mut j, mut s) = converge(
            CLUSTER_CONVERGE,
            "2026-09-15T16:11:00Z",
            "2026-09-15T16:15:00Z",
            json!({ "build_head": "34db709" }),
        );
        s[0].metadata["result"] = json!("exit-code");
        j.metadata["outcome"] = json!("failed");
        let rows = sidings(&[train(&[&c], false)], std::slice::from_ref(&c), &[(j, s)]);
        let l = landing_of(&rows, &c);
        assert!(awaiting(&l).contains("ended exit-code"), "{l:?}");
    }

    /// The window begins after the merge: nothing can be said, and the
    /// row says that rather than "converging".
    #[test]
    fn a_merge_older_than_the_converge_window_reads_unread_not_converging() {
        let c = car(Some("config"));
        let after = converge(
            CLUSTER_CONVERGE,
            "2026-09-15T18:00:00Z",
            "2026-09-15T18:00:01Z",
            json!({ "unchanged": "9999999" }),
        );
        let rows = sidings(&[train(&[&c], true)], std::slice::from_ref(&c), &[after]);
        match landing_of(&rows, &c) {
            Landing::Unread { why } => assert!(why.contains("2026-09-15T18:00:00Z"), "{why}"),
            other => panic!("expected unread, got {other:?}"),
        }
        // No packet at all is unread too — an empty window is not a
        // reading of "not yet".
        let rows = sidings(&[train(&[&c], true)], std::slice::from_ref(&c), &[]);
        assert!(matches!(landing_of(&rows, &c), Landing::Unread { .. }));
    }

    // ---- infra: the host converge packets ----

    fn host(kind: &str, node: &str, sha: &str, closed: &str) -> (Job, Vec<Step>) {
        converge(
            kind,
            "2026-09-15T16:00:00Z",
            closed,
            json!({ "converge_sha": sha, "node_id": node }),
        )
    }

    /// The measured case: forge converged six minutes after the merge,
    /// boss-gcp fourteen minutes after the IMAGE ROLL. The car lands
    /// only when every host the window shows has reported the merge,
    /// and until then names the host it waits for.
    #[test]
    fn an_infra_car_lands_when_every_host_has_converged_on_its_merge() {
        let c = car(Some("infra"));
        let forge = host(
            "maintenance-forge-converge",
            "forge",
            "34db7093e3b7f9394ff469b92a301fb39a731a80",
            "2026-09-15T16:16:07Z",
        );
        let gcp_before = host(
            "maintenance-boss-gcp-converge",
            "boss-gcp",
            "6c13191bbda28805976716d00bceb12a55aef0c7",
            "2026-09-15T16:04:23Z",
        );
        let rows = sidings(
            &[train(&[&c], true)],
            std::slice::from_ref(&c),
            &[forge.clone(), gcp_before.clone()],
        );
        let l = landing_of(&rows, &c);
        assert!(awaiting(&l).contains("boss-gcp"), "{l:?}");
        assert!(awaiting(&l).contains("last reported 6c13191"), "{l:?}");
        assert!(
            !awaiting(&l).contains("forge —"),
            "forge is not awaited: {l:?}"
        );

        let gcp = host(
            "maintenance-boss-gcp-converge",
            "boss-gcp",
            "34db7093e3b7f9394ff469b92a301fb39a731a80",
            "2026-09-15T16:34:23Z",
        );
        let rows = sidings(
            &[train(&[&c], true)],
            std::slice::from_ref(&c),
            &[forge, gcp_before, gcp],
        );
        let l = landing_of(&rows, &c);
        assert_eq!(
            l,
            Landing::Landed {
                evidence: evidence(&l).to_string(),
                at: Some("2026-09-15T16:34:23Z".into())
            }
        );
        assert!(
            evidence(&l).starts_with("boss-gcp, forge converged on 34db709"),
            "{l:?}"
        );
    }

    /// A host converge packet from before `node_id` was recorded still
    /// speaks for the host its kind names.
    #[test]
    fn a_host_packet_without_a_node_id_speaks_for_the_host_its_kind_names() {
        let c = car(Some("infra"));
        let mut forge = host(
            "maintenance-forge-converge",
            "",
            "34db7093e3b7f9394ff469b92a301fb39a731a80",
            "2026-09-15T16:16:07Z",
        );
        forge.1[0]
            .metadata
            .as_object_mut()
            .unwrap()
            .remove("node_id");
        let rows = sidings(&[train(&[&c], true)], std::slice::from_ref(&c), &[forge]);
        assert!(evidence(&landing_of(&rows, &c)).starts_with("forge converged"));
    }

    #[test]
    fn an_infra_car_with_no_host_packets_reads_unread() {
        let c = car(Some("infra"));
        let rows = sidings(&[train(&[&c], true)], std::slice::from_ref(&c), &[]);
        assert!(matches!(landing_of(&rows, &c), Landing::Unread { .. }));
    }

    // ---- the lane's shape ----

    /// An unmerged train has nothing live to judge; a merged one names
    /// each boarded car in the window, on its own siding, with the
    /// train it rode.
    #[test]
    fn only_merged_trains_contribute_rows_and_each_car_names_its_train() {
        let a = car(Some("config"));
        let b = car(Some("software"));
        let outside = car(Some("data"));
        let merged = train(&[&a, &b, &outside], true);
        let unmerged = {
            let j = job(
                "pr-train",
                JobStatus::Open,
                json!({ "boarded_jobs": [b.id.to_string()] }),
            );
            (j, vec![step_ready("merged", "Merged into main")])
        };
        let rows = sidings(&[unmerged, merged.clone()], &[a.clone(), b.clone()], &[]);
        assert_eq!(
            rows.len(),
            2,
            "two cars in the window, one train merged: {rows:?}"
        );
        assert!(rows.iter().all(|r| r.train == merged.0.id.to_string()));
        assert_eq!(rows[0].branch.as_deref(), Some("fix/x"));
        assert_eq!(
            rows.iter().map(|r| r.channel.as_str()).collect::<Vec<_>>(),
            vec!["config", "software"]
        );
    }

    /// The wire shape the floor reads: a tagged `landing`.
    #[test]
    fn a_siding_row_serializes_its_landing_tagged_by_kind() {
        let c = car(Some("software"));
        let rows = sidings(&[train(&[&c], true)], std::slice::from_ref(&c), &[]);
        let v = serde_json::to_value(&rows[0]).unwrap();
        assert_eq!(v["channel"], "software");
        assert_eq!(v["landing"]["kind"], "landed");
        assert_eq!(v["landing"]["at"], "2026-09-15T16:20:45Z");
        let rows = sidings(&[train(&[&c], false)], std::slice::from_ref(&c), &[]);
        let v = serde_json::to_value(&rows[0]).unwrap();
        assert_eq!(v["landing"]["kind"], "converging");
        assert!(v["landing"]["awaiting"].is_string());
    }
}
