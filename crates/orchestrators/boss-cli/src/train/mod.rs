//! `boss train` — drive the pr-train Workflow.
//!
//! Ported from `infra/train/conductor.py` (directive 26d61c97: no
//! python runs the BOSS system — the conductor's logic now lives in
//! the same `boss` binary the box already ships). The semantics, the
//! journal lines, and the incident history below are the python
//! conductor's, carried over intact.
//!
//! The train is the cadence: changes accumulate on branches with their
//! ship-a-change Jobs parked at `review`, and twice a day this runs and
//! does the batching a person used to do by discipline. Two phases:
//!
//!  1. RECONCILE — for every OPEN pr-train Job, record whatever evidence
//!     arrived since the last run: the CI verdict (polled from the
//!     forge), the merge (observed, never assumed), and the deploys that
//!     carried the merge out. Steps close only when the conductor holds
//!     the evidence in hand; a train whose PR nobody merged just stays
//!     open, visibly. Once a train has arrived, the sweep deletes each
//!     landed car's branch from the forge — on the job record's
//!     evidence, because squash-merged trains leave no git ancestry to
//!     prove a landing (see `deletable_branches`), and only while the
//!     branch still points at the head that boarded (`sweep_guard`).
//!     The train's OWN branch comes off the same way at arrival
//!     (`arrival_branch_to_delete`): the internal forge keeps merged
//!     PR heads, and 62 stale `train/*` branches piled up in a week
//!     before arrival owned its own housekeeping (ab3fa473).
//!
//!  2. BOARD — collect the ship-a-change Jobs that are ready (review
//!     step ready/active, a branch pushed to the fork, not already on a
//!     train), assemble one train branch by merging each on top of
//!     origin/main, run the CONSIST CHECK over the assembled tree
//!     (`consist_check` — seconds of cheap text lints, because a
//!     per-branch gate cannot see a failure that exists only in the
//!     combination), push it, THEN open this window's train Job, then
//!     ONE batched PR.
//!     A branch that does not merge cleanly is skipped, named on the Job,
//!     and left for the next train. An empty window — or a consist the
//!     check refused — departs nothing and OPENS NO PACKET: the journal
//!     says why in one `no train departed` line and each car keeps its
//!     own `skip_reason`. A refusal strikes no car (see "A BOARD THAT
//!     DEPARTS NO TRAIN OPENS NO PACKET" for the ten hours that bought
//!     the ordering).
//!
//! Two trees, deliberately:
//!   - assembly happens in a dedicated clone (BOSS_TRAIN_HOME/repo) —
//!     never in the dev working tree, which may hold a session's
//!     half-built work;
//!   - deploys run from the dev tree (/opt/boss) only when it is clean
//!     and on main; otherwise the deploy is left pending with the reason
//!     recorded, and the next run retries.
//!
//! Talks to jobs-api directly with an actor header (the gateway strips
//! inbound identity, same as boss-step.sh). Steps are addressed by
//! `spec_slug` with a title fallback for steps that predate the column.
//!
//! THIS FILE EXECUTES. IT DOES NOT DECIDE.
//! The thresholds, budgets and rosters the conductor works to are
//! registry data, read once per invocation and threaded through
//! `Conductor::policy` — see `crate::delivery_policy`, which is the only
//! place a policy number is written down in Rust (and only as the
//! fallback for a registry that cannot be reached). If you are looking
//! for "how many strikes hold a car" or "which lints run on a consist",
//! it is a row, not a constant (docs/design/delivery-as-protocol.md).

use std::collections::{BTreeSet, HashMap};
use std::fs::{self, File, TryLockError};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;
use boss_jobs::car;
use boss_jobs::delivery::DeliveryPolicyRow;
use chrono::{DateTime, Utc};
use reqwest::Method;
use serde_json::{Map, Value, json};

use crate::delivery_policy::{self, DeliveryPolicy};
use crate::host_readiness;

// The regions of the conductor, one file each, cut along the
// `// ----` banners this file carried while it was one 16,271-line
// module (consolidation H1, 2026-09-18). Every item keeps its path:
// `crate::train::X` resolves through the re-exports below.
mod boarding;
mod cars;
mod conductor;
mod consist;
mod dock_regate;
mod entry;
mod forge;
mod jobs_api;
mod judged_red;
mod merge_lost;
mod preflight;
mod red_verdict;
mod stranded;
mod sweep;
mod verdicts;

pub(crate) use boarding::*;
pub(crate) use cars::*;
use conductor::*;
pub(crate) use consist::*;
pub(crate) use entry::*;
pub(crate) use forge::*;
pub(crate) use jobs_api::*;
pub(crate) use judged_red::*;
pub(crate) use merge_lost::*;
pub(crate) use preflight::*;
pub(crate) use red_verdict::*;
pub(crate) use stranded::*;
pub(crate) use sweep::*;
pub(crate) use verdicts::*;

/// The conductor's own id, for the rows a train OWNS (its Job's
/// `owner_id`, the `actor` stamp on it). One definition, in
/// `identity` — aliased here rather than re-spelled, because a
/// constant cannot drift from itself (CLAUDE.md §9a).
const ACTOR: &str = crate::identity::CONDUCTOR;

/// The registry row an `/api/delivery/policy/*` response carries. Both
/// endpoints answer `null` for "no such policy" — an ANSWER, not an
/// error, so it arrives here as `Ok(None)` and the caller falls back.
fn row_of_policy(body: Option<Value>) -> Result<Option<DeliveryPolicyRow>> {
    match body {
        None | Some(Value::Null) => Ok(None),
        Some(v) => serde_json::from_value(v)
            .map(Some)
            .context("parsing the delivery policy row"),
    }
}

/// The `x-boss-user` the CONDUCTOR's own calls carry.
///
/// This is the conductor's loop, so the conductor is the actor —
/// unless something named a different one (`BOSS_ACTOR`), which is
/// how a human running `boss train cancel` by hand signs as
/// themselves and how the unit states its automation identity out
/// loud. Every OTHER verb signs its caller instead and refuses to
/// guess: see `identity`, and backlog 5083d6f5 for what guessing
/// recorded.
pub(crate) fn boss_user() -> String {
    crate::identity::header(&crate::identity::conductor())
}

/// The train's `<branch>@<short sha>`, as the ASSEMBLE STEP recorded it
/// (`completed assemble … train_ref`). Read there first; the train's own
/// metadata second, where older fixtures and an older reader put it.
/// The first live train gate (71098905, 2026-09-13 03:30Z) was not
/// filed because the launcher read only the train metadata: "the train
/// carries no train_ref" — three passes running, then CI alone.
pub(crate) fn train_ref_of(train: &Value) -> Option<&str> {
    find_step(train, "assemble", "Assemble the train branch")
        .and_then(|s| s.get("metadata"))
        .and_then(|m| m.get("train_ref"))
        .and_then(Value::as_str)
        .filter(|r| !r.is_empty())
        .or_else(|| {
            train
                .pointer("/metadata/train_ref")
                .and_then(Value::as_str)
                .filter(|r| !r.is_empty())
        })
}

/// PURE: whether the boarding pass owes this train its gate before the
/// pass ends (backlog 95c349a5). Measured 2026-09-14 on four trains, PR
/// open -> gate filed was 2.5–8.5 minutes of pure waiting, because the
/// gate was filed from the ci-step block on the NEXT reconcile pass.
/// Yes once the train carries what `launch_train_gate` reads — a
/// `train_ref` on its assemble step and the PR recorded on its pr step
/// — and no gate-run yet. A train that already carries one is the ci
/// block's to read; a train missing the ref would fail the launch on
/// "carries no train_ref" and spend one of `MAX_LAUNCH_FAILURES` on it.
pub(crate) fn gate_due_at_boarding(train: &Value) -> bool {
    let pr_recorded = find_step(train, "pr", "Open the batched PR")
        .and_then(|s| s.get("metadata"))
        .and_then(|m| m.get("pr_url"))
        .and_then(Value::as_str)
        .is_some_and(|u| !u.is_empty());
    let unfiled = train
        .pointer(&format!("/metadata/{}", crate::train_gate::KEY_RUN))
        .and_then(Value::as_str)
        .is_none_or(str::is_empty);
    pr_recorded && train_ref_of(train).is_some() && unfiled
}

pub(crate) fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

struct Config {
    jobs: String,
    gh_repo: String,
    head_owner: String,
    fork_url: String,
    upstream_url: String,
    home: String,
    clone: String,
    /// Which forge adapter is active (BOSS_TRAIN_FORGE: `github` or
    /// `forgejo`). Stored on the config so decisions that hinge on
    /// WHICH forge — the arrival branch cleanup only runs against the
    /// internal forge, whose merged PR heads outlive their PRs — read
    /// the same answer `make_forge` acted on.
    forge_kind: String,
    /// The train protocol revision (directive 27ab7680): under the
    /// forge, CI-green trains merge themselves — GitHub was a 10-hour
    /// permission wall on an all-green train, and the human wall in
    /// this protocol is the car review at parking, not a mechanical
    /// click at landing.
    auto_merge: bool,
    /// The drift sentinel's deliberate escape hatch
    /// (BOSS_TRAIN_ALLOW_LOCAL_JOBS=1): accept a loopback jobs URL.
    /// Test harnesses and demo boxes only — on a real box the jobs
    /// system of record lives elsewhere (incident c4b4a6b0).
    allow_local_jobs: bool,
    /// Hours the PR may sit without CI producing ANY verdict before the
    /// conductor says so (BOSS_TRAIN_CI_HOURS, default 2). David's
    /// number, 2026-08-15: roughly twice the measured p90 of pr->ci.
    ci_hours: i64,
    /// Minutes after the merge before an unconverged cluster is a loud
    /// packet instead of a quiet wait (BOSS_TRAIN_CONVERGE_ALARM_MINS,
    /// default 30 — David's number, 2026-08-19; the healthy path
    /// measures ~10-20 min of image build + rollout, the failure this
    /// exists for measured six silent hours).
    converge_alarm_mins: i64,
    /// Minutes a HAND-gated green (no `--park-*` intent) may sit with no
    /// ship-a-change car before the conductor files a stranded-green
    /// alarm (BOSS_TRAIN_STRANDED_ALARM_MINS, default 45). Elapsed time
    /// is the only signal for a human's forgotten green: 45 min clears a
    /// gate-then-`boss park` gap with margin while still catching the
    /// strand long before its base drifts and a blind rescue reverts
    /// landed work. Measured from the VERDICT, not from the gate's start
    /// — see [`freshest_green`] for what that cost when it was not.
    stranded_alarm_mins: i64,
    /// Minutes of grace for `jobs.auto-park` on a green that DID carry a
    /// park intent (BOSS_TRAIN_AUTO_PARK_GRACE_MINS, default 10). The
    /// handler fires on the `step.done.gate-verdict` event and files in
    /// 0.6 SECONDS (measured 2026-09-09 over the 12 most recent
    /// intent-carrying greens on the system of record), so this is not a
    /// wait for the happy path — it is room for a dispatcher restart or
    /// a redelivery before the alarm says the handler failed.
    auto_park_grace_mins: i64,
    /// Release a red train's consist automatically once it has stalled
    /// (BOSS_TRAIN_AUTO_CANCEL, default ON — set to `0` to disable).
    /// On by default because the failure it prevents is a pipeline that
    /// stops at the first red and stays stopped until a human looks;
    /// the kill switch exists so an operator debugging a consist can
    /// keep it on the rails without editing code.
    auto_cancel: bool,
    /// The estate node id of the host CI runs on (BOSS_TRAIN_CI_HOST,
    /// deliberately no default — a wrong guess would gate boardings on
    /// the wrong box's disk, and a wrong id answers "never observed"
    /// instead of erroring). Absent means the pre-boarding host check
    /// is skipped, with one journal line, so a deployment that has not
    /// configured it behaves exactly as before.
    ci_host: Option<String>,
    /// THE TRAIN GATE (design 128b5496): the runner manifest `boss gate`
    /// renders and the namespace its Jobs run in — the conductor files a
    /// gate-run of the train branch when it opens the PR and reads the
    /// train as green only when CI AND that gate are (BOSS_GATE_MANIFEST,
    /// BOSS_GATE_NAMESPACE; see `train_gate`).
    gate_manifest: String,
    gate_namespace: String,
    /// BOSS_TRAIN_GATE_REQUIRED=1: a train waits for its gate however
    /// long; otherwise a gate that cannot be FILED falls back to CI
    /// alone, stamped (`train_gate::REQUIRED_ENV` — off until car 3 of
    /// 128b5496 stops CI running the Rust checks).
    gate_required: bool,
    dry: bool,
}

impl Config {
    fn from_env(dry: bool) -> Self {
        // THE FORGE IS THE SOURCE; GITHUB IS A PERIODIC BACKUP.
        //
        // David, 2026-08-30: "We aren't supposed to have any github
        // dependency. Our git is a private internal server so that we
        // can include its operations directly. Github should only be
        // thought of as a periodic, safety backup." The tree already
        // said as much at the arrival sweep — "GitHub is the mirror,
        // never the source (27ab7680)" — but these defaults said the
        // opposite, and defaults are what an unconfigured run gets.
        //
        // All three mattered. `git clone $upstream_url` is how a fresh
        // conductor bootstraps, so the default made a NEW conductor pull
        // its source from the backup. The `fork` remote pointed at a
        // GitHub fork that the forgejo path does not use. And
        // `forge_kind` defaulting to `github` is what selected the
        // GitHub adapter over a forge clone whenever the systemd unit's
        // environment was absent — a bare `boss train cancel` released
        // every car and then failed on `gh pr close http://10.20.0.15
        // :3000/...`, leaving two trains half-cancelled (b9801aff).
        let forge_base = env_or("BOSS_TRAIN_FORGE_URL", "http://10.20.0.15:3000");
        let forge_repo = env_or("BOSS_TRAIN_FORGE_REPO", "david/boss");
        // Still read, and only for the BACKUP: it names the public
        // mirror the GitHub adapter would address. Nothing on the
        // source path uses it.
        let gh_repo = env_or("BOSS_TRAIN_GH_REPO", "algedonic-dev/boss");
        let home = env_or("BOSS_TRAIN_HOME", "/var/lib/boss-train");
        Config {
            jobs: env_or("BOSS_JOBS_URL", "http://127.0.0.1:7900"),
            head_owner: env_or("BOSS_TRAIN_HEAD_OWNER", "dauld"),
            // Under the forge there is no separate fork: the conductor
            // pushes train branches to the same repository it reads,
            // which is what the running conductor's `fork` remote
            // already points at.
            fork_url: env_or(
                "BOSS_TRAIN_FORK_URL",
                &format!("{forge_base}/{forge_repo}.git"),
            ),
            upstream_url: env_or(
                "BOSS_TRAIN_UPSTREAM_URL",
                &format!("{forge_base}/{forge_repo}.git"),
            ),
            clone: format!("{home}/repo"),
            forge_kind: env_or("BOSS_TRAIN_FORGE", "forgejo"),
            auto_merge: std::env::var("BOSS_TRAIN_AUTO_MERGE").as_deref() == Ok("1"),
            allow_local_jobs: std::env::var("BOSS_TRAIN_ALLOW_LOCAL_JOBS").as_deref() == Ok("1"),
            ci_hours: env_or("BOSS_TRAIN_CI_HOURS", "2").parse().unwrap_or(2),
            converge_alarm_mins: env_or("BOSS_TRAIN_CONVERGE_ALARM_MINS", "30")
                .parse()
                .unwrap_or(30),
            stranded_alarm_mins: env_or("BOSS_TRAIN_STRANDED_ALARM_MINS", "45")
                .parse()
                .unwrap_or(45),
            auto_park_grace_mins: env_or("BOSS_TRAIN_AUTO_PARK_GRACE_MINS", "10")
                .parse()
                .unwrap_or(10),
            auto_cancel: std::env::var("BOSS_TRAIN_AUTO_CANCEL").as_deref() != Ok("0"),
            ci_host: std::env::var("BOSS_TRAIN_CI_HOST")
                .ok()
                .filter(|s| !s.trim().is_empty()),
            gate_manifest: env_or(
                crate::train_gate::MANIFEST_ENV,
                crate::train_gate::DEFAULT_MANIFEST,
            ),
            gate_namespace: env_or(
                crate::train_gate::NAMESPACE_ENV,
                crate::train_gate::DEFAULT_NAMESPACE,
            ),
            gate_required: std::env::var(crate::train_gate::REQUIRED_ENV).as_deref() == Ok("1"),
            gh_repo,
            home,
            dry,
        }
    }
}

fn log(msg: impl std::fmt::Display) {
    println!("conductor: {msg}");
}

/// Run a command capturing output; error on non-zero exit with the
/// same message shape the python `sh()` raised.
fn sh_in(cwd: Option<&Path>, check: bool, args: &[&str]) -> Result<Output> {
    // git carries the forge credential on the command itself
    // (git_auth); every other program runs bare.
    let mut cmd = if args[0] == "git" {
        crate::git_auth::command()
    } else {
        Command::new(args[0])
    };
    cmd.args(&args[1..]);
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    let out = cmd
        .output()
        .with_context(|| format!("spawning {}", args.join(" ")))?;
    if check && !out.status.success() {
        bail!(
            "{}: rc={}\n{}",
            args.join(" "),
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(out)
}

fn sh(args: &[&str]) -> Result<Output> {
    sh_in(None, true, args)
}

fn sh_unchecked(args: &[&str]) -> Result<Output> {
    sh_in(None, false, args)
}

fn stdout_str(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The ref is on the assemble step, where the assembly writes it;
    /// the train metadata is the older home and still read.
    #[test]
    fn the_train_ref_is_read_off_the_assemble_step_first() {
        let live = serde_json::json!({
            "metadata": { "boarded_jobs": ["c1"] },
            "steps": [
                { "spec_slug": "assemble", "title": "Assemble the train branch", "status": "completed",
                  "metadata": { "train_ref": "train/20260913-0327@1a74705f" } }
            ]
        });
        assert_eq!(train_ref_of(&live), Some("train/20260913-0327@1a74705f"));
        let old =
            serde_json::json!({ "metadata": { "train_ref": "train/x@abcdef1" }, "steps": [] });
        assert_eq!(train_ref_of(&old), Some("train/x@abcdef1"));
        let none = serde_json::json!({ "metadata": {}, "steps": [{ "spec_slug": "assemble", "status": "ready", "metadata": {} }] });
        assert_eq!(train_ref_of(&none), None);
    }

    // -- the train gate is filed in the pass that opens the PR ---------
    //
    // Measured 2026-09-14 on four trains (backlog 95c349a5): PR open ->
    // gate filed took 8m30s, 4m29s, 6m30s, 2m30s — pure waiting, because
    // the gate was filed from the ci-step block on the NEXT reconcile
    // pass. The boarding pass owes the gate itself, and this predicate
    // is what it asks before filing: it must say yes to exactly the
    // train boarding leaves behind, and no to a train that already
    // carries a gate-run (the ci block reads that one) or one whose
    // record is not yet complete enough to launch from.
    #[test]
    fn a_freshly_boarded_train_is_owed_its_gate_in_the_same_pass() {
        let boarded = json!({
            "id": "t-1",
            "metadata": { "boarded_jobs": ["c1"] },
            "steps": [
                { "spec_slug": "assemble", "title": "Assemble the train branch", "status": "completed",
                  "metadata": { "train_ref": "train/20260914-1726@1a74705f" } },
                { "spec_slug": "pr", "title": "Open the batched PR", "status": "completed",
                  "metadata": { "pr_url": "https://forge.example/david/boss/pulls/361" } }
            ]
        });
        assert!(
            gate_due_at_boarding(&boarded),
            "a train with its PR open, a train_ref and no gate-run is owed a gate before the boarding pass ends"
        );

        // Already filed — this pass or a previous one: the ci block reads it.
        let mut filed = boarded.clone();
        filed["metadata"][crate::train_gate::KEY_RUN] = json!("g-1");
        assert!(!gate_due_at_boarding(&filed));

        // The PR was never recorded: nothing to gate against yet.
        let mut no_pr = boarded.clone();
        no_pr["steps"][1]["status"] = json!("ready");
        no_pr["steps"][1]["metadata"] = json!({});
        assert!(!gate_due_at_boarding(&no_pr));

        // No train_ref: the launch would fail on "carries no train_ref"
        // and burn one of the three launch attempts for nothing.
        let mut no_ref = boarded.clone();
        no_ref["steps"][0]["metadata"] = json!({});
        assert!(!gate_due_at_boarding(&no_ref));
    }
}

/// Fixtures shared by more than one region's tests: the compiled
/// delivery policy, a landed car, an arrived train, and the real-git
/// scratch clones the publish and consist tests drive.
#[cfg(test)]
mod test_support {
    use super::*;

    /// THE POLICY EVERY TEST BELOW DECIDES BY, unless it is deliberately
    /// exercising a different one. It is the compiled fallback, which is
    /// exactly what the seeded registry row parses to
    /// (`delivery_policy::db_tests::the_seeded_policy_equals_the_compiled_fallback`)
    /// — so these tests pin the same behaviour they pinned before the
    /// numbers moved.
    pub(super) fn policy() -> DeliveryPolicy {
        DeliveryPolicy::compiled()
    }

    /// A boarded car whose bookkeeping completed: closed with the
    /// `merged` outcome stamped by the terminal close.
    pub(super) fn landed_car(id: &str, branch: &str) -> serde_json::Value {
        json!({
            "id": id,
            "status": "closed",
            "metadata": {"branch": branch, "outcome": "merged", "merged": "true"},
        })
    }

    pub(super) fn arrived_train() -> serde_json::Value {
        json!({
            "id": "train-77",
            "status": "closed",
            "metadata": {
                "boarded_jobs": ["car-1", "car-2"],
                "left_behind": [
                    {"car_id_short": "car-3-id", "reason": "conflict: src/a.rs"}
                ],
            },
            "steps": [
                {"spec_slug": "collect", "title": "Collect what is ready to board",
                 "status": "completed",
                 "metadata": {"completed_at": "2026-08-13T06:00:00Z"}},
                {"spec_slug": "merged", "title": "Merged into main",
                 "status": "completed",
                 "metadata": {"completed_at": "2026-08-13T06:05:00Z",
                              "merge_ref": "abc1234def56"}},
                {"spec_slug": "deployed", "title": "Deployed to the playground",
                 "status": "completed",
                 "metadata": {"completed_at": "2026-08-13T06:12:00Z",
                              "deployed": "main@abc1234; 0 applied; services: prod; web: deployed"}},
                {"spec_slug": "arrived", "title": "Train arrived",
                 "status": "completed",
                 "metadata": {"completed_at": "2026-08-13T06:20:00Z"}},
            ],
        })
    }

    /// The `arrived_train` fixture plus the subject the cleanup keys
    /// on — the train's own `train/*` branch.
    pub(super) fn arrived_train_with_branch() -> Value {
        let mut train = arrived_train();
        train["subject"] = json!({"subject_kind": "custom", "id": "train/20260820-0600"});
        train
    }

    /// Removes its directory on drop, so a panicking test does not
    /// leave repositories in /tmp.
    pub(super) struct Scratch(pub(super) std::path::PathBuf);
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    pub(super) fn git_ok(dir: &std::path::Path, args: &[&str]) {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap_or_else(|e| panic!("git {args:?}: {e}"));
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// A bare fork, a bare origin, and a clone wired to both — the
    /// conductor's actual shape.
    pub(super) fn clone_fixture(name: &str) -> (Scratch, std::path::PathBuf) {
        // `scratch_dir`, not a fixed `/tmp/boss-pcb-<name>`: the root
        // carries the uid and the pid. /tmp is 1777 and this pod runs the
        // suite as root AND as the gate's uid 65534, so a fixed name
        // belongs to whichever ran first and is unwritable for the other.
        let root = boss_testing::scratch::scratch_dir(&format!("boss-pcb-{name}"));
        let guard = Scratch(root.clone());
        let clone = root.join("clone");
        for bare in ["fork.git", "origin.git"] {
            let p = root.join(bare);
            std::fs::create_dir_all(&p).expect("mkdir bare");
            let out = std::process::Command::new("git")
                .args(["init", "--bare", "-b", "main"])
                .arg(&p)
                .output()
                .expect("init bare");
            assert!(out.status.success());
        }
        std::fs::create_dir_all(&clone).expect("mkdir clone");
        git_ok(&clone, &["init", "-b", "main"]);
        git_ok(&clone, &["config", "user.email", "t@example.com"]);
        git_ok(&clone, &["config", "user.name", "t"]);
        std::fs::write(clone.join("README"), name).expect("write");
        git_ok(&clone, &["add", "-A"]);
        git_ok(&clone, &["commit", "-qm", "base"]);
        git_ok(
            &clone,
            &[
                "remote",
                "add",
                "fork",
                root.join("fork.git").to_str().expect("utf8"),
            ],
        );
        git_ok(
            &clone,
            &[
                "remote",
                "add",
                "origin",
                root.join("origin.git").to_str().expect("utf8"),
            ],
        );
        git_ok(&clone, &["push", "-q", "origin", "main"]);
        git_ok(&clone, &["push", "-q", "fork", "main"]);
        (guard, clone)
    }

    pub(super) fn on_fork(clone: &std::path::Path, branch: &str) -> bool {
        std::process::Command::new("git")
            .arg("-C")
            .arg(clone)
            .args([
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("fork/{branch}"),
            ])
            .output()
            .expect("rev-parse")
            .status
            .success()
    }

    pub(super) fn commit_branch(clone: &std::path::Path, branch: &str) {
        git_ok(clone, &["checkout", "-q", "-b", branch]);
        std::fs::write(clone.join("x"), branch).expect("write");
        git_ok(clone, &["add", "-A"]);
        git_ok(clone, &["commit", "-qm", "work"]);
        git_ok(clone, &["checkout", "-q", "main"]);
    }

    /// One more commit on an existing branch, leaving `main` checked
    /// out — what fixing a car looks like in the conductor's clone.
    pub(super) fn advance_branch(clone: &std::path::Path, branch: &str, marker: &str) {
        git_ok(clone, &["checkout", "-q", branch]);
        std::fs::write(clone.join("x"), marker).expect("write");
        git_ok(clone, &["add", "-A"]);
        git_ok(clone, &["commit", "-qm", marker]);
        git_ok(clone, &["checkout", "-q", "main"]);
    }

    pub(super) fn rev(clone: &std::path::Path, refname: &str) -> String {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(clone)
            .args(["rev-parse", refname])
            .output()
            .expect("rev-parse");
        assert!(out.status.success(), "rev-parse {refname}");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }
}
