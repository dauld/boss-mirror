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
use chrono::{DateTime, FixedOffset, Utc};
use reqwest::Method;
use serde_json::{Map, Value, json};

use crate::delivery_policy::{self, DeliveryPolicy};
use crate::host_readiness;

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

/// Which slice of the conductor to run. `Run` is the timer entry
/// (reconcile + board); the others are the standalone verbs the
/// python argv flags (`--preflight`, `--reconcile-only`) selected.
/// `Cancel` is the operator's judgment call on a train that will not
/// arrive — close the PR unmerged, release the cars, record why.
pub enum Phase {
    Preflight,
    Reconcile,
    Board,
    Run,
    Cancel { handle: String, reason: String },
}

/// What a contended lock MEANS for the phase that gave up on it.
///
/// Whether to WAIT first is [`lock_wait_budget`]'s question. This one
/// is asked only once waiting is over: did leaving finish the job, or
/// abandon a request nobody else will pick up?
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Contended {
    /// The holder is doing this very work right now: reconcile and
    /// board are the standing loop, and a standalone preflight has
    /// nothing further to prove while the locomotive is demonstrably
    /// pulling. Nothing was abandoned — so a phase that left AT ONCE
    /// leaves at 0. (A starvable phase that spent its whole budget
    /// still never ran, and exits [`LOCK_CONTENDED_EXIT`] for that
    /// separate reason.)
    Covered,
    /// Leave at [`LOCK_CONTENDED_EXIT`], saying what did not happen.
    /// Nothing else in the system will do it.
    Abandoned(String),
}

/// Classify a contended lock for `phase`.
///
/// `Cancel` is the one phase carrying an operator's specific request:
/// release THESE cars from THAT train. No other run will cancel it, so
/// leaving abandons the request — and returning `Ok(())` from here told
/// the operator the opposite. That cost nothing the two times the verb
/// was run on 2026-09-10 because the lock happened to be free; had it
/// not been, the reader of "cancelled" would have believed three cars
/// were back on the dock while they were still aboard a dead train
/// holding the track.
pub(crate) fn contended(phase: &Phase) -> Contended {
    match phase {
        Phase::Cancel { handle, .. } => Contended::Abandoned(format!(
            "train {handle} NOT cancelled: another conductor run holds the lock. \
             No car was released and the PR is still open — re-run the cancel once \
             the run in progress finishes."
        )),
        Phase::Preflight | Phase::Reconcile | Phase::Board | Phase::Run => Contended::Covered,
    }
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
    deploy_tree: String,
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
            // Default `/opt/boss` is the boss-gcp conductor's playground
            // tree and stays unchanged. Set BOSS_TRAIN_DEPLOY_TREE="" to
            // mean "the deploy happens elsewhere (the cluster converge),
            // not here" — the intended config for a cluster-resident
            // conductor, which has no such tree and no sudo. See
            // `playground_deploy_disabled` and `deploy`.
            deploy_tree: env_or("BOSS_TRAIN_DEPLOY_TREE", "/opt/boss"),
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

// ---------------------------------------------------------------------------
// Phase 0 — pre-flight the locomotive
//
// The 2026-08-10 18:01 window crashed before boarding: a sudo probe had
// left root-owned objects in the clone, and the conductor's fetch died
// at the moment the window opened. The consist had been rehearsed; the
// locomotive had not. Every entry (including the 10-minute reconcile,
// which is thereby the early-warning cadence) proves the clone healthy
// before touching train state, and a sick locomotive exits 3 — loud in
// the unit's status — instead of surfacing at departure time.
// ---------------------------------------------------------------------------

/// The conductor's effective uid. std exposes no geteuid, and the
/// workspace carries no libc-level dependency worth adding for one
/// call; POSIX `id -u` prints exactly this.
fn euid() -> Result<u32> {
    let out = sh(&["id", "-u"])?;
    stdout_str(&out).trim().parse().context("parsing `id -u`")
}

/// Collect files under `dir` not owned by uid `me` — the recursive
/// half of python's os.walk. A directory that refuses a read is
/// skipped (os.walk's default); a file gone before lstat is skipped
/// too — gc'd mid-walk; ownership of what remains is what matters.
fn walk_foreign(dir: &Path, me: u32, foreign: &mut Vec<PathBuf>) -> Result<()> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Ok(());
    };
    for entry in entries {
        let Ok(entry) = entry else { continue };
        let path = entry.path();
        let meta = match path.symlink_metadata() {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e).context(format!("lstat {}", path.display())),
        };
        if meta.is_dir() {
            walk_foreign(&path, me, foreign)?;
        } else if meta.uid() != me {
            foreign.push(path);
        }
    }
    Ok(())
}

/// Host of an http(s) URL — scheme, userinfo, port, and path all
/// stripped. Enough to ask "is this loopback?" without a URL crate.
fn url_host(url: &str) -> &str {
    let rest = url.split("://").nth(1).unwrap_or(url);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host = authority.rsplit('@').next().unwrap_or_default();
    match host.strip_prefix('[') {
        Some(v6) => v6.split(']').next().unwrap_or_default(),
        None => host.split(':').next().unwrap_or_default(),
    }
}

/// The drift sentinel (split-brain incident c4b4a6b0): BOSS_JOBS_URL
/// defaulted to localhost on a cutover box and the conductor silently
/// booked a whole window's trains on the wrong instance. A loopback
/// jobs URL is a preflight problem unless the box declares that a
/// local jobs-api is the point — BOSS_TRAIN_ALLOW_LOCAL_JOBS=1, set
/// deliberately by test harnesses and demo boxes.
pub(crate) fn local_jobs_problem(jobs_url: &str, allow_local: bool) -> Option<String> {
    if allow_local {
        return None;
    }
    let host = url_host(jobs_url);
    let loopback = host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback());
    loopback.then(|| {
        format!(
            "BOSS_JOBS_URL resolves to loopback ({jobs_url}) — bookkeeping must target \
             the jobs system of record, not this box (split-brain incident c4b4a6b0); \
             set BOSS_TRAIN_ALLOW_LOCAL_JOBS=1 only where a local jobs-api is the point"
        )
    })
}

/// Return the list of problems; empty means the locomotive is fit.
fn preflight(cfg: &Config) -> Result<Vec<String>> {
    let mut problems = Vec::new();
    // Every git command below carries the forge credential on itself
    // (git_auth::command) — nothing to configure first, nothing written
    // to this or any other user's git config.
    // The drift sentinel runs first, clone or no clone: a conductor
    // whose bookkeeping would land on this box instead of the system
    // of record must not pull at all.
    if let Some(p) = local_jobs_problem(&cfg.jobs, cfg.allow_local_jobs) {
        problems.push(p);
    }
    // The invariant is OWNERSHIP, not uid zero: the conductor must run
    // as the clone's owner. The original flat refuse-root check said
    // the same thing only on the box where the service user is not
    // root — in a CI container every process IS root and the fixture
    // clone is root-owned, which is perfectly consistent. The
    // foreign-owned walk below enforces the real rule in both worlds:
    // root over the service user's clone still fails (every object is
    // foreign to euid 0), and the poisoning incident this guards
    // against stays guarded.
    let git_dir = Path::new(&cfg.clone).join(".git");
    if !git_dir.is_dir() {
        log("preflight: no clone yet — first boarding will create it");
        return Ok(problems);
    }
    let me = euid()?;
    let mut foreign = Vec::new();
    walk_foreign(&git_dir, me, &mut foreign)?;
    if !foreign.is_empty() {
        let shown = foreign
            .iter()
            .take(3)
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", ");
        problems.push(format!(
            "{} object(s) in the clone not owned by uid {me} (e.g. {shown}) — \
             a foreign-uid run has poisoned {}",
            foreign.len(),
            cfg.clone
        ));
    }
    for remote in ["origin", "fork"] {
        let r = sh_unchecked(&[
            "git",
            "-C",
            &cfg.clone,
            "fetch",
            remote,
            "--prune",
            "--dry-run",
        ])?;
        if !r.status.success() {
            let stderr = String::from_utf8_lossy(&r.stderr);
            let stderr = stderr.trim();
            let detail = if stderr.is_empty() {
                format!("rc={}", r.status.code().unwrap_or(-1))
            } else {
                stderr.lines().last().unwrap_or_default().to_string()
            };
            problems.push(format!("dry fetch of {remote} failed: {detail}"));
        }
    }
    // THE ADAPTER MUST MATCH THE REMOTE IT WILL BE POINTED AT.
    //
    // `BOSS_TRAIN_FORGE` defaults to `github`, so a conductor verb run
    // without the systemd unit's environment selects the GitHub adapter
    // over a clone whose remotes are the internal forge. Nothing says
    // so: the command runs, and `gh pr close http://10.20.0.15:3000/...`
    // fails at the END with "none of the git remotes ... point to a
    // known GitHub host" — after `boss train cancel` has already
    // released every car. Two trains were left half-cancelled that way
    // on 2026-08-27 (b9801aff), and preflight is where the packet's own
    // correction says the assertion belongs.
    let origin = sh_unchecked(&["git", "-C", &cfg.clone, "remote", "get-url", "origin"]);
    if let Ok(o) = origin
        && o.status.success()
        && let Some(p) = forge_mismatch(&cfg.forge_kind, String::from_utf8_lossy(&o.stdout).trim())
    {
        problems.push(p);
    }
    Ok(problems)
}

/// Does the selected forge adapter match the remote it will act on?
///
/// PURE, because the refusal has to be exactly right: a false positive
/// here stops the conductor entirely. Only a definite contradiction
/// counts — the GitHub adapter over a non-GitHub origin, or the Forgejo
/// adapter over github.com. Anything unrecognised is left alone.
pub(crate) fn forge_mismatch(forge_kind: &str, origin_url: &str) -> Option<String> {
    // A LOCAL PATH IS NOT A FORGE, so it cannot contradict one. The
    // first version of this check refused any non-GitHub origin, which
    // failed `healthy_clone_passes` — that fixture points origin at
    // /tmp/…/upstream.git with no forge configured, and there is nothing
    // wrong with it. The gate caught it, which is the outcome this
    // function's own doc comment asks for: a false positive here stops
    // every train, so it is worse than the bug.
    let addressable = origin_url.contains("://") || origin_url.contains('@');
    if !addressable {
        return None;
    }
    let is_github = origin_url.contains("github.com");
    match forge_kind {
        "github" if !is_github && !origin_url.is_empty() => Some(format!(
            "forge adapter is `github` (BOSS_TRAIN_FORGE unset defaults to it) but origin is \
             {origin_url}, which is not a GitHub host. Every forge call would fail — and a \
             cancel fails only AFTER releasing its cars. Set the conductor's environment: \
             BOSS_TRAIN_FORGE=forgejo BOSS_TRAIN_FORGE_URL=http://10.20.0.15:3000 \
             BOSS_TRAIN_FORGE_REPO=david/boss \
             BOSS_TRAIN_FORGE_TOKEN_FILE=/etc/boss-train/forge.token"
        )),
        "forgejo" if is_github => Some(format!(
            "forge adapter is `forgejo` but origin is {origin_url}, a GitHub host — the \
             adapter would post to a forge that does not hold this repository."
        )),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// The consist check — proving the ASSEMBLED tree before spending CI on it
//
// Pre-flight above checks the LOCOMOTIVE. This checks the CONSIST, and
// it exists because of a number: train arrival went 100% (08-20) → 40%
// (08-23) → 0% (08-24) as cars-per-train rose. Every failure in the last
// two days was a COMBINATION failure — invisible to a per-branch gate,
// because on each branch alone there was nothing wrong:
//
//   - two cars each added `infra/postgres/schema/153-*.sql`. Unique on
//     either branch; a duplicate the moment they were assembled.
//   - a new lint (`infra/lint/one-palette.sh`) and the mocked spec that
//     has to NAME the forbidden pattern in order to test it rode the
//     same train. The lint failed on the spec.
//
// Each cost roughly 90 minutes of CI to learn ONE bit, plus a cancel,
// plus a strike on every innocent car aboard. So the conductor asks the
// cheap questions itself, against the tree it just assembled, before it
// spends anything. Three rules keep it from becoming the thing it is
// meant to save:
//
//   - CHEAP ONLY. Text lints, run out of the assembled `infra/lint/`.
//     No cargo, no bun, no database. Measured: the 23 included scripts
//     total ~9 seconds. The full gate is what CI is for; this is not a
//     second gate and must never grow into one.
//   - DISCOVERED, NOT LISTED. Every `infra/lint/*.sh` in the ASSEMBLED
//     tree runs, minus a named exclusion set — which is the whole point
//     of the second failure above: the lint that catches the next
//     combination failure may be arriving ON THE TRAIN, and no
//     hand-picked pair in this file could have seen it.
//   - TAME WHEN IT BREAKS ITSELF. Missing, unrunnable, or over budget
//     is a logged warning and the train departs. A preflight that
//     becomes a new way to block every train costs more than it saves.
//
// What it deliberately does NOT do is decide whose fault the failure
// was. Nobody's: each car was green alone. So a refusal opens no PR,
// strikes no car, and leaves every one of them boardable carrying a
// reason that names the lint and the files.
// ---------------------------------------------------------------------------

// The roster, the exclusions and the three budgets used to be four
// constants here. They are policy — every one of them is a question
// somebody could reasonably answer differently tomorrow — and they now
// arrive as a `DeliveryPolicy` resolved once per invocation from the
// registry (`crate::delivery_policy`). What stayed here is the
// mechanism: walk the tree, run bash, read exit codes, decide.

/// What one cheap lint said about the assembled tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LintResult {
    Passed,
    /// Non-zero exit: the tree is bad. Carries the combined
    /// stdout+stderr, because half these scripts report on stderr.
    Failed(String),
    /// The check itself could not run. Never a refusal — see the
    /// third rule above.
    Unrunnable(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LintRun {
    pub(crate) name: String,
    pub(crate) result: LintResult,
}

/// A lint that disagreed with the assembled tree, with the files its
/// own output named (best effort — a hint on the car, not a claim).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LintFailure {
    pub(crate) name: String,
    pub(crate) output: String,
    pub(crate) files: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ConsistVerdict {
    Proceed {
        ran: usize,
        warnings: Vec<String>,
    },
    Refuse {
        failed: Vec<LintFailure>,
        ran: usize,
        warnings: Vec<String>,
    },
}

impl ConsistVerdict {
    pub(crate) fn ran(&self) -> usize {
        match self {
            ConsistVerdict::Proceed { ran, .. } | ConsistVerdict::Refuse { ran, .. } => *ran,
        }
    }

    pub(crate) fn warnings(&self) -> &[String] {
        match self {
            ConsistVerdict::Proceed { warnings, .. } | ConsistVerdict::Refuse { warnings, .. } => {
                warnings
            }
        }
    }
}

/// The files a lint's output names, so a refusal can say WHICH files
/// collided rather than only which check complained. Deliberately a
/// text heuristic over every lint's output rather than a parser per
/// lint: the checks are free to say whatever they say, and a hint that
/// is occasionally empty is worth more than a parser that must be
/// extended for every new script.
///
/// A token counts as a filename when it ends in a short alphabetic
/// extension — which keeps `Cargo.toml` and `153-a.sql` and drops
/// `v1.2`, `0.8`, and sentences ending in a full stop.
pub(crate) fn files_named_in(output: &str, budget: usize) -> Vec<String> {
    let mut named: Vec<String> = Vec::new();
    for token in output.split_whitespace() {
        let token =
            token.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '/' && c != '.');
        let Some((stem, ext)) = token.rsplit_once('.') else {
            continue;
        };
        let looks_like_a_file = !stem.is_empty()
            && (1..=6).contains(&ext.len())
            && ext.starts_with(|c: char| c.is_ascii_alphabetic())
            && ext.chars().all(|c| c.is_ascii_alphanumeric());
        if !looks_like_a_file {
            continue;
        }
        if !named.iter().any(|f| f == token) {
            named.push(token.to_string());
        }
        if named.len() == budget {
            break;
        }
    }
    named
}

/// ONE reason string, journal and Job alike — the chip the yard
/// renders and the line the operator greps must never tell different
/// stories (the rule `skip_reason_conflict` already follows, down to
/// the file budget: this lands on `metadata.skip_reason`, which
/// PacketCard renders as "LEFT BEHIND — <reason>", so the reason does
/// not repeat the words the chip already says).
///
/// The last clause is the point of the whole car. A car that reads
/// this did nothing wrong, and must not be treated — by a person or by
/// the boarding rules — as if it had.
pub(crate) fn consist_refusal_reason(failed: &[LintFailure], file_budget: usize) -> String {
    let Some(first) = failed.first() else {
        return "consist check refused, no failing check named".to_string();
    };
    let mut files = first.files.join(", ");
    if files.len() > file_budget {
        files = format!("{} files", first.files.len());
    }
    let named = if files.is_empty() {
        String::new()
    } else {
        format!(" ({files})")
    };
    let others = match failed.len() {
        0 | 1 => String::new(),
        n => format!(" +{} more check(s)", n - 1),
    };
    format!(
        "consist check: {} failed on the assembled tree{named}{others} — a combination failure, \
         not this car's fault",
        first.name
    )
}

/// Which `infra/lint/*.sh` of the assembled tree this check runs, in
/// a deterministic order (sorted, so two runs over one tree ask the
/// same questions in the same sequence). The roster is the directory
/// minus the policy's exclusions — nothing in code to edit when a lint
/// lands, and nothing in code to edit when an exclusion changes either.
fn cheap_lints(tree: &Path, policy: &DeliveryPolicy) -> Result<Vec<PathBuf>> {
    let dir = tree.join("infra/lint");
    let mut scripts: Vec<PathBuf> = fs::read_dir(&dir)
        .with_context(|| format!("reading {}", dir.display()))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "sh"))
        .filter(|p| {
            let name = p
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            !policy.excludes(&name)
        })
        .collect();
    scripts.sort();
    Ok(scripts)
}

/// Run one lint against `tree`. `bash <script>` rather than executing
/// it directly: the checkout may not carry the executable bit, and
/// every one of these scripts is a bash script that locates the repo
/// root from its own path.
fn run_one_lint(tree: &Path, script: &Path, output_budget: usize) -> LintResult {
    if !script.is_file() {
        return LintResult::Unrunnable("not a readable file".to_string());
    }
    let out = Command::new("bash").arg(script).current_dir(tree).output();
    let out = match out {
        Ok(out) => out,
        Err(e) => return LintResult::Unrunnable(format!("could not spawn bash: {e}")),
    };
    match out.status.code() {
        Some(0) => LintResult::Passed,
        // The shell's own "I could not run that" codes. 127 is what a
        // dangling script name produces, and reading that as "the tree
        // is bad" would turn a deleted file into a stopped railway.
        Some(126) => LintResult::Unrunnable("not executable by the shell (126)".to_string()),
        Some(127) => LintResult::Unrunnable("command not found (127)".to_string()),
        _ => {
            let mut text = format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            text.truncate(
                text.char_indices()
                    .nth(output_budget)
                    .map_or(text.len(), |(i, _)| i),
            );
            LintResult::Failed(text.trim().to_string())
        }
    }
}

/// The decision. Pure over what the lints said, so the verdict is
/// testable without a tree and the tree-walking stays in one place.
pub(crate) fn consist_verdict(runs: &[LintRun], files_named: usize) -> ConsistVerdict {
    let mut warnings = Vec::new();
    let mut failed = Vec::new();
    let mut ran = 0;
    for run in runs {
        match &run.result {
            LintResult::Passed => ran += 1,
            LintResult::Failed(output) => {
                ran += 1;
                failed.push(LintFailure {
                    name: run.name.clone(),
                    output: output.clone(),
                    files: files_named_in(output, files_named),
                });
            }
            // Named with the `.sh` back on: what could not run is a
            // FILE, and the operator's next move is to look for it.
            LintResult::Unrunnable(why) => {
                warnings.push(format!("{}.sh could not run ({why})", run.name));
            }
        }
    }
    if failed.is_empty() {
        ConsistVerdict::Proceed { ran, warnings }
    } else {
        ConsistVerdict::Refuse {
            failed,
            ran,
            warnings,
        }
    }
}

/// Point the clone's `origin/main` at CURRENT forge main before the
/// consist lints resolve their baseline against it.
///
/// THE FALSE POSITIVE THIS CLOSES (2026-09-06). Several cheap lints
/// (`a-kind-bundle-does-not-tighten.sh`, `migrations-append-only.sh`)
/// compute their baseline as `git merge-base(<trunk ref>, HEAD)` in
/// the conductor's OWN clone, where the trunk ref is `origin/main`.
/// A car merge pulls the car's ancestry — the last-landed main — into
/// the assembled HEAD, but the clone's `origin/main` ref only advances
/// on a fetch. When a prior train has landed and this board has not
/// re-fetched since, that ref lags behind the tree it is being
/// compared against, so the merge-base falls to a commit BEFORE the
/// last train's changes and the lint reads those already-landed
/// changes as if this consist introduced them. Observed: a
/// `bill-approval.po_id is now required` refusal on a consist whose
/// `step_types.toml` was byte-identical to main — nobody's car at
/// fault, the whole train refused, boarding blocked until the next
/// reconcile-fetch happened to freshen the ref.
///
/// BEST-EFFORT, NON-FATAL. A broken fetch must never hold a train (the
/// rule: conductor loop writes must not be fatal). A failed freshen
/// (a network blip, a missing remote) LOGS and returns; the lints then
/// resolve against the ref as it already stands, which is exactly
/// today's behaviour — so a failed freshen is never worse than not
/// trying. The failure is logged, not `.ok()`-swallowed, so it stays
/// visible. `origin` is the remote the conductor's clone fetches in
/// `ensure_clone` and checks out the train branch from, and the remote
/// the lints' `origin/main` trunk ref is fed by.
fn freshen_trunk(clone: &str) {
    match sh_unchecked(&[
        "git",
        "-C",
        clone,
        "fetch",
        "--quiet",
        "origin",
        "+refs/heads/main:refs/remotes/origin/main",
    ]) {
        Ok(out) if out.status.success() => {}
        Ok(out) => log(format!(
            "consist check: could not freshen origin/main (git fetch rc={}) — the lints will \
             resolve their baseline against the trunk ref as it stands: {}",
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stderr).trim()
        )),
        Err(e) => log(format!(
            "consist check: could not run the trunk fetch — the lints will resolve their \
             baseline against the trunk ref as it stands: {e}"
        )),
    }
}

/// Ask every cheap lint in the assembled tree what it thinks, then
/// decide. Every failure mode of this function itself lands as a
/// warning on a `Proceed`.
///
/// All the checks run, not just up to the first failure: they are
/// seconds each, and learning ONE bit per attempt is precisely the
/// cost this exists to stop paying.
pub(crate) fn consist_check(tree: &Path, policy: &DeliveryPolicy) -> ConsistVerdict {
    let scripts = match cheap_lints(tree, policy) {
        Ok(s) if s.is_empty() => {
            return ConsistVerdict::Proceed {
                ran: 0,
                warnings: vec![
                    "no lint scripts in the assembled tree — nothing cheap to ask".to_string(),
                ],
            };
        }
        Ok(s) => s,
        Err(e) => {
            return ConsistVerdict::Proceed {
                ran: 0,
                warnings: vec![format!("could not list the tree's lints ({e})")],
            };
        }
    };

    let started = std::time::Instant::now();
    let total = scripts.len();
    let mut runs = Vec::with_capacity(total);
    for (done, script) in scripts.iter().enumerate() {
        let name = script
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .trim_end_matches(".sh")
            .to_string();
        runs.push(LintRun {
            name,
            result: run_one_lint(tree, script, policy.consist_output_budget),
        });
        if started.elapsed() > policy.consist_budget && done + 1 < total {
            let mut verdict = consist_verdict(&runs, policy.consist_files_named);
            let spent = started.elapsed().as_secs();
            let note = format!(
                "budget spent ({spent}s) after {} of {total} checks — going on what ran",
                done + 1
            );
            match &mut verdict {
                ConsistVerdict::Proceed { warnings, .. }
                | ConsistVerdict::Refuse { warnings, .. } => warnings.push(note),
            }
            return verdict;
        }
    }
    consist_verdict(&runs, policy.consist_files_named)
}

// ---------------------------------------------------------------------------
// A BOARD THAT DEPARTS NO TRAIN OPENS NO PACKET
//
// Ten hours of delivery went to this on 2026-09-10 (backlog 4860aff8).
// ONE car sat on the dock that the consist check refused. The board
// cadence is queue-depth based and re-fires every 60 seconds while the
// dock stays deep, and every attempt OPENED a pr-train Job and then
// cancelled it: `pr-train` total passed 982, the newest 100 rows all
// closed inside 99 minutes, 89 `outcome: cancelled`, and 100 of 100
// with no `boarded_jobs`. That is ~1,440 phantom packets a day from one
// unboardable car. It turned the branch sweep's 50-row window over in
// under an hour (02069932) and it lied to every window read — the yard,
// `boss orient`, and any count of how many trains ran.
//
// The refusal itself was already right: no PR opened, no CI spent. What
// it did not skip was the PACKET, because the Job was created BEFORE
// the check ran — for one reason, that the refusal wanted somewhere to
// record itself. The record is cheaper than that:
//
//   - each car keeps its own `skip_reason`, and for a consist refusal
//     the structured `consist_refusal` too — the lint output, on the
//     car whose boarding it blocked, where the operator already looks;
//   - the journal gets ONE line, this one, which names the reason and
//     says outright that no packet was opened, so nobody goes hunting
//     the yard for a train that never existed.
//
// A packet is a fact that something happened; a train that never
// departed is not one. And until it was cancelled it also HELD THE
// TRACK, so a cancel lost to one API blip wedged boarding behind a
// train that had never left the yard.
// ---------------------------------------------------------------------------

/// Why a board attempt departed no train. Every one of these used to
/// open a pr-train Job and immediately cancel it through the `empty`
/// marker; none of them opens anything now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum NoDeparture {
    /// The CI host is short of what a run needs — an infrastructure
    /// refusal, decided before a single car was collected.
    HostShort { reason: String },
    /// Nothing was parked and ready when the window opened.
    NothingParked,
    /// Every candidate conflicted on the assembled tree.
    AllConflicted { branches: String },
    /// The consist check refused the assembled tree. Nobody's car is at
    /// fault — each was green on its own branch — so every car stays
    /// boardable and unstruck.
    ConsistRefused { reason: String, cars: usize },
    /// Every car on the dock is held on a declared ordering edge
    /// (`metadata.boards_after`) — see `boards_after_outcome`.
    ///
    /// TWO LISTS, BECAUSE THEY ASK DIFFERENT THINGS OF THE READER. A car
    /// waiting on a predecessor that is still in flight needs nobody: the
    /// next board is 60 seconds away and it departs on its own. A car
    /// whose predecessor was abandoned, or whose edge names no Job at
    /// all, can NEVER depart on the edge it declares, and the window will
    /// refuse identically forever until a person clears it. An operator
    /// reading this line is deciding whether the pipeline is stuck, so
    /// the line has to answer that and not merely report a hold
    /// (d3320278).
    HeldOnEdges { cars: String, needs_human: String },
}

/// The journal line a refused board leaves. It is the only record of
/// the window now, so it carries the reason AND the fact that no packet
/// was opened; a reader who greps `no train departed` gets every
/// non-departure, whatever refused it.
pub(crate) fn no_departure_line(refusal: &NoDeparture) -> String {
    match refusal {
        NoDeparture::HostShort { reason } => format!(
            "no train departed — boarding refused before any car was collected: {reason}. \
             No train packet opened, no PR, no CI spent."
        ),
        NoDeparture::NothingParked => "no train departed — no car was parked and ready when the \
             window opened: an idle window, not a failure. No train packet opened."
            .to_string(),
        NoDeparture::AllConflicted { branches } => format!(
            "no train departed — every candidate was skipped on merge conflicts: {branches}. \
             No train packet opened, no PR, no CI spent; each car carries its own skip_reason."
        ),
        NoDeparture::ConsistRefused { reason, cars } => format!(
            "no train departed — {reason}. No train packet opened, no PR, no CI spent — \
             {cars} car(s) stay boardable and unstruck."
        ),
        NoDeparture::HeldOnEdges { cars, needs_human } if needs_human.is_empty() => format!(
            "no train departed — every car on the dock is held on the ordering edge it \
             declared: {cars}. No train packet opened. NOTHING NEEDS DOING: each boards by \
             itself on a later window, once the car it named has landed."
        ),
        NoDeparture::HeldOnEdges { cars, needs_human } => format!(
            "no train departed — every car on the dock is held on the ordering edge it \
             declared: {cars}. No train packet opened. A HUMAN IS NEEDED for {needs_human}: \
             that edge can never be satisfied, so this window will refuse identically until \
             someone clears it — each car's own skip_reason names which."
        ),
    }
}

// ---------------------------------------------------------------------------
// THE DECLARED ORDERING EDGE — a car names the car it boards AFTER
//
// Four cars were held by hand on 2026-09-10 and every one of them was an
// ordering constraint (backlog d3320278; design doc 364f892e, all three
// questions accepted as proposed). `fix/the-reclaim-follows-the-build`
// was gated `--hold` and parked by hand when the dock happened to be
// empty, purely so it would board SOLO as a gate-blind ci.yml change;
// `feat/a-deleted-manifest-leaves-no-object` spent a day waiting for a
// human to notice both halves of a two-part sequence were satisfied. The
// operator WAS the mechanism, and the mechanism was re-reading the dock.
//
// FILTER, DO NOT PLAN (decision 3). Boarding skips a car whose declared
// predecessor has not landed; it does not compute a multi-train
// sequence. A plan buys nothing the filter does not, because the next
// board happens on its own inside the cooldown. If the dock ever grows
// deep enough that planning is visibly better, that is a second car.
//
// AN UNSATISFIABLE EDGE REFUSES AND IS NAMED (decision 2). Boarding
// anyway after a timeout was rejected: it reintroduces exactly the
// collision the edge prevents. So the refusal is LOUD on every 60-second
// board attempt, and it is FOUR refusals rather than one, because an
// operator reading the dock is deciding whether the pipeline is stuck:
//
//   still in flight  — nobody does anything; it departs on its own
//   landed           — satisfied; the car boards (no refusal at all)
//   abandoned        — a human must break the edge; it will never clear
//   no such Job      — a human must fix the reference
//
// Collapsing those into "dependency not met" would build the quiet hold
// this feature exists to remove (CLAUDE.md §Diagnosis: quiet is a loan
// against the next diagnosis).
//
// AND IT MUST NEVER FREEZE A LANDING. A fallible read inside the board
// loop that refused everything it could not evaluate would stop every
// train, and the gate does not run the conductor, so the gate would not
// catch it. Every way the read can fail therefore DEGRADES TOWARD
// BOARDING, loudly: `Unreadable` is a journal line and the car rides.
// The edge's job is to stop a known collision, not to become a new way
// for the pipeline to stop.
// ---------------------------------------------------------------------------

/// The structured marker a boarding-edge hold leaves on its `left_behind`
/// entry, so the window's own refusal line is composed from DATA and not
/// from sniffing the reason string back apart.
pub(crate) const EDGE_HOLD: &str = "edge_hold";
/// The predecessor is still in flight — self-clearing, no action.
pub(crate) const EDGE_HOLD_WAITING: &str = "waiting";
/// The edge can never be satisfied as declared — a person must act.
pub(crate) const EDGE_HOLD_NEEDS_HUMAN: &str = "needs_human";

/// What the conductor managed to learn about a car's declared
/// predecessor. `Unreadable` is a first-class answer, not an error:
/// "I could not ask" must be distinguishable from "it is not there".
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Predecessor {
    /// The Job came back, judged by `boss_jobs::car`'s own predicates so
    /// the conductor cannot disagree with the rest of the system about
    /// what "landed" means.
    Found(Value),
    /// The jobs API answered that there is no such Job.
    Absent,
    /// The read itself failed — a blip, an outage, a malformed body.
    Unreadable(String),
}

/// Why a car may not board on its declared edge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EdgeHold {
    /// The reason, journal and Job chip alike — ONE string, as every
    /// other skip reason here is.
    pub(crate) reason: String,
    /// `EDGE_HOLD_WAITING` or `EDGE_HOLD_NEEDS_HUMAN`.
    pub(crate) kind: &'static str,
}

/// What boarding should do about a car's declared edge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum EdgeOutcome {
    /// Board it: the edge is satisfied, or there is none.
    Board,
    /// Board it, and SAY why the edge could not be judged. Fail-open by
    /// design — see the section comment.
    BoardUnjudged(String),
    /// Leave it behind, with the reason named on it.
    Hold(EdgeHold),
}

/// The predecessor a car declared, if it declared one. A blank value is
/// no declaration — the metadata door deletes a null key but a `""` is a
/// real stored value, and `jobs_clear_waiting` shows `""` is how an edge
/// gets cleared in practice.
pub(crate) fn declared_predecessor(car: &Value) -> Option<String> {
    car.pointer(&format!("/metadata/{}", car::BOARDS_AFTER))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
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
/// place rather than asserting a mood. The three states are core's
/// (`is_boarded` / `is_parked` / `is_building`), in the order a car
/// passes through them backwards.
fn in_flight_at(pred: &Value) -> String {
    if car::is_boarded(pred) {
        let train = pred
            .pointer("/metadata/train")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if train.is_empty() {
            "aboard a train".to_string()
        } else {
            format!("aboard train {}", id8(train))
        }
    } else if car::is_parked(pred) {
        "parked at the dock".to_string()
    } else if car::is_building(pred) {
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
pub(crate) fn boards_after_outcome(declared: &str, pred: &Predecessor) -> EdgeOutcome {
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
                car::BOARDS_AFTER
            ),
            kind: EDGE_HOLD_NEEDS_HUMAN,
        }),
        Predecessor::Found(p) if car::is_landed(p) => EdgeOutcome::Board,
        Predecessor::Found(p) if car::is_open(p) => EdgeOutcome::Hold(EdgeHold {
            reason: format!(
                "boards after {}, which is STILL IN FLIGHT ({}) — no action needed; this \
                 car boards on a later window once that one lands",
                predecessor_name(declared, Some(p)),
                in_flight_at(p)
            ),
            kind: EDGE_HOLD_WAITING,
        }),
        Predecessor::Found(p) => EdgeOutcome::Hold(EdgeHold {
            reason: format!(
                "boards after {}, which was ABANDONED ({}) — the edge can never be \
                 satisfied; a human must clear metadata.{} on this car, or abandon it too",
                predecessor_name(declared, Some(p)),
                spent_as(p),
                car::BOARDS_AFTER
            ),
            kind: EDGE_HOLD_NEEDS_HUMAN,
        }),
    }
}

/// PURE: is this jobs-API error the ANSWER "there is no such Job",
/// rather than "I could not ask"? The difference decides whether a car
/// is held for a person to fix or boarded anyway, so it is worth a named
/// function and a test instead of an inline `.contains` at the call site.
///
/// THE PAIR THIS PINS (CLAUDE.md §9a). The status lives in
/// `ApiFailure.kind` as `Failure::Http(404)`, but `api` returns
/// `anyhow::Error` — the classifier's structure is gone by the time a
/// caller sees it, and surfacing it would mean a second shape for every
/// call in this file. So the status is read back out of the message
/// `api_once` builds, and `a_404_is_read_back_out_of_the_message_api_once_builds`
/// constructs that message the way `api_once` does so the two cannot
/// drift silently. Collapsing this properly means `api` handing back the
/// `Failure`; that is a wider change than this car.
pub(crate) fn is_no_such_job(err: &anyhow::Error) -> bool {
    err.chain().any(|c| c.to_string().contains("HTTP 404"))
}

/// PURE: the refusal an empty candidate list deserves.
///
/// `NothingParked` says "an idle window, not a failure" — true when the
/// dock is empty, a LIE when the dock is full of cars the edge filter
/// held, and the difference is exactly what an operator is reading the
/// line to learn. Composed from the structured `EDGE_HOLD` marker on
/// each left-behind entry, never from re-parsing the reason prose.
pub(crate) fn empty_dock_refusal(left_behind: &[Value]) -> NoDeparture {
    let named = |want: &str| -> Vec<String> {
        left_behind
            .iter()
            .filter(|e| e.get(EDGE_HOLD).and_then(Value::as_str) == Some(want))
            .filter_map(|e| e.get("car_id_short").and_then(Value::as_str))
            .map(str::to_string)
            .collect()
    };
    let waiting = named(EDGE_HOLD_WAITING);
    let needs_human = named(EDGE_HOLD_NEEDS_HUMAN);
    if waiting.is_empty() && needs_human.is_empty() {
        return NoDeparture::NothingParked;
    }
    let mut cars = waiting;
    cars.extend(needs_human.iter().cloned());
    NoDeparture::HeldOnEdges {
        cars: cars.join(", "),
        needs_human: needs_human.join(", "),
    }
}

// ---------------------------------------------------------------------------
// The jobs-API blip guard
//
// The cluster is the system of record, and it rolls. Twice on
// 2026-08-13 a reconcile hit `Connection refused` to the jobs API
// mid-converge and returned rc=1 for the whole verb — right to refuse
// to act blind, needlessly brittle about an outage that lasted
// seconds (the cadence loop's dock probe held the queue-depth rules
// for that tick on the same blip). A bounded retry covers the roll.
//
// Two rules keep it from papering over anything real: a 4xx is an
// ANSWER and is never retried, and every retry journals one line, so
// blips stay measurable instead of invisible.
// ---------------------------------------------------------------------------

/// What a failed jobs-API attempt was. The classifier reads this and
/// nothing else — pure, and pinned by tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Failure {
    /// The connection never established — refused, DNS, TLS. Proof
    /// that the request did not reach the system of record.
    Connect,
    /// A timeout, or a response that died mid-body: nothing usable
    /// came back, and whether the write happened is UNKNOWN.
    Ambiguous,
    /// The jobs API answered, with this status.
    Http(u16),
    /// The answer arrived and was unusable — an unparseable body.
    Malformed,
}

/// Retry, or surface? Two rules, and the second is the one that keeps
/// the retry honest:
///
///   - a 4xx is an ANSWER (a 422 is the SoR saying no, and asking the
///     same question three times does not change it); only transport
///     failures and 5xx are blips;
///   - a blip that leaves the write AMBIGUOUS may only be re-sent when
///     the call is idempotent. Re-POSTing an ambiguous create is how
///     one blip becomes two train Jobs. A refused connection is not
///     ambiguous — nothing was received — so anything may go again,
///     which is exactly the production case this exists for.
pub(crate) fn retryable(method: &Method, failure: &Failure) -> bool {
    let idempotent = matches!(
        *method,
        Method::GET | Method::PUT | Method::DELETE | Method::HEAD
    );
    match failure {
        Failure::Connect => true,
        Failure::Ambiguous => idempotent,
        Failure::Http(status) => idempotent && (500..600).contains(status),
        Failure::Malformed => false,
    }
}

/// A reqwest error, classified. Connect / timeout / mid-flight body
/// failures are the blips a rolling SoR produces; a builder or
/// redirect error is a misconfiguration, and retrying one just burns
/// the window three times over.
fn classify_transport(e: &reqwest::Error) -> Failure {
    if e.is_connect() {
        Failure::Connect
    } else if e.is_timeout() || e.is_request() || e.is_body() {
        Failure::Ambiguous
    } else {
        Failure::Malformed
    }
}

/// A jobs-API call that did not succeed: what it was (for the
/// classifier) and the error to surface once the retries run out.
pub(crate) struct ApiFailure {
    pub(crate) kind: Failure,
    pub(crate) cause: anyhow::Error,
}

impl ApiFailure {
    /// A reqwest failure — classified by what reqwest says went wrong.
    pub(crate) fn transport(e: reqwest::Error, context: String) -> Self {
        ApiFailure {
            kind: classify_transport(&e),
            cause: anyhow::Error::new(e).context(context),
        }
    }
}

/// The bounded retry: how many attempts in total, and the first wait
/// between them (each further wait doubles).
#[derive(Debug, Clone, Copy)]
pub(crate) struct RetryPolicy {
    pub(crate) attempts: u32,
    pub(crate) base: Duration,
}

/// The jobs-API policy: 3 attempts, 2s then 4s. A pod roll is over
/// inside that budget, and a jobs API still refusing after it is an
/// outage the verb should surface rather than paper over.
pub(crate) const JOBS_API_RETRY: RetryPolicy = RetryPolicy {
    attempts: 3,
    base: Duration::from_secs(2),
};

impl RetryPolicy {
    /// The wait before attempt `n + 1`, doubling from `base`.
    pub(crate) fn backoff(&self, attempt: u32) -> Duration {
        self.base * 2u32.pow(attempt.saturating_sub(1).min(16))
    }

    /// The same decisions with no waiting — the tests' policy, so the
    /// retry semantics get pinned without spending the backoff.
    #[cfg(test)]
    pub(crate) const fn immediate(attempts: u32) -> Self {
        RetryPolicy {
            attempts,
            base: Duration::ZERO,
        }
    }
}

/// The one-line cause of a blip: the INNERMOST error, which is where
/// the fact lives ("Connection refused (os error 61)") — the layers
/// above it just repeat the url the journal line already implies.
///
/// `budget` is policy (`delivery_policy.blip_cause_budget`); the
/// truncation is mechanism.
pub(crate) fn short_cause(err: &anyhow::Error, budget: usize) -> String {
    let innermost = err
        .chain()
        .last()
        .map(|c| c.to_string())
        .unwrap_or_default();
    let line = innermost.lines().next().unwrap_or_default().trim();
    if line.chars().count() <= budget {
        return line.to_string();
    }
    format!("{}…", line.chars().take(budget).collect::<String>())
}

/// Run `op` until it succeeds, its failure turns out to be an answer
/// rather than a blip, or the attempt budget runs out. Every retry
/// journals one line through `journal` — the caller's idiom, so the
/// conductor's blips read `conductor: ` and the cadence loop's read
/// `cadence: `. (`+ Sync` because the cadence loop's spawned verb
/// tasks record their outcome through this door, and a future that
/// crosses `tokio::spawn` must be `Send`.)
pub(crate) async fn retrying<T, F, Fut>(
    policy: &RetryPolicy,
    method: &Method,
    cause_budget: usize,
    journal: &(dyn Fn(&str) + Sync),
    mut op: F,
) -> Result<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = std::result::Result<T, ApiFailure>>,
{
    let mut attempt = 1u32;
    loop {
        let failure = match op().await {
            Ok(v) => return Ok(v),
            Err(f) => f,
        };
        if attempt >= policy.attempts || !retryable(method, &failure.kind) {
            return Err(failure.cause);
        }
        journal(&format!(
            "jobs API blip ({attempt}/{}): {}",
            policy.attempts,
            short_cause(&failure.cause, cause_budget)
        ));
        tokio::time::sleep(policy.backoff(attempt)).await;
        attempt += 1;
    }
}

// ---------------------------------------------------------------------------
// jobs-api helpers
// ---------------------------------------------------------------------------

/// Does this open train hold the single track? Only while it is
/// PRE-MERGE — its `merged` step not yet completed. A merged train's
/// content is already on main: the next consist merges on top of it,
/// and the earlier train's `converged` step is designed to arrive by
/// ANCESTRY (`convergence_verdict` accepts a running commit that is a
/// descendant of its merge), so a later train landing first is what
/// converges it, not what strands it. Holding the track for a merged
/// train deadlocked delivery twice on 2026-09-07 (f3796323): a merged
/// train whose sha bricked its boot sat at `converged` forever, and the
/// fix-forward car could not board because the track was "occupied"
/// by the very train it would have converged.
///
/// Fails closed: a row with no `steps`, or no `merged` step, is
/// pre-merge and holds — unknown is not "clear".
///
/// Twin: `boss_jobs::yard::holds_the_track` reads the same rule off
/// typed steps for the yard board. The typed read model and this JSON
/// cannot share one signature, so each test names its twin
/// (CLAUDE.md §9a).
pub(crate) fn holds_the_track(train: &Value) -> bool {
    // `is_none_or`: no step, or no readable status, holds (fail closed);
    // only a `completed` merge releases.
    find_step(train, "merged", "Merged into main")
        .and_then(|s| s.get("status"))
        .and_then(Value::as_str)
        .is_none_or(|s| s != "completed")
}

/// The train holding the track, named for the journal, or None when
/// the track is clear. Only a PRE-MERGE open train occupies it
/// (`holds_the_track`): a second consist assembled while the first is
/// still merging would merge onto a main the first is about to change
/// (a8c6773b); once the first has merged, the next consist merges on
/// top of it and converges it by ancestry. Arrived and cancelled trains
/// are closed, so both clear the track — a red train that stall-cancels
/// never blocks the next one. Names the first holder.
pub(crate) fn track_occupied_by(open_trains: &[Value]) -> Option<String> {
    open_trains.iter().find(|t| holds_the_track(t)).map(|t| {
        let title = t
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or("an unnamed train");
        let id = t.get("id").and_then(Value::as_str).unwrap_or("?");
        format!("{title} ({})", &id[..id.len().min(8)])
    })
}

/// The list body, whether or not the endpoint wrapped it in
/// `{"data": [...]}`.
pub(crate) fn rows(resp: Option<Value>) -> Result<Vec<Value>> {
    let resp = resp.ok_or_else(|| anyhow!("empty response for a list call"))?;
    let list = match resp {
        Value::Object(mut o) if o.contains_key("data") => o.remove("data").unwrap_or(Value::Null),
        other => other,
    };
    match list {
        Value::Array(v) => Ok(v),
        other => bail!("expected a job list, got: {other}"),
    }
}

/// One page of a paginated `/api/jobs` read. Kept at the historical
/// 100 so a backlog that fits under a page still makes exactly one
/// call: the defect this file fixes is paging PAST a page, not making
/// the page bigger.
pub(crate) const PAGE_LIMIT: usize = 100;

/// The `total` a list response carries — the DB-wide count of rows
/// matching the filter AND the caller's policy scope, authoritative
/// over any single page's length (`http/jobs.rs` builds it beside
/// `data`). A body without it is an error, never zero: zero is what a
/// wrong deployment answers (CLAUDE.md §Doors), and this number decides
/// whether every matching car has been read.
pub(crate) fn list_total(body: &Value) -> Result<usize> {
    let total = body
        .get("total")
        .and_then(Value::as_u64)
        .ok_or_else(|| anyhow!("list response carries no `total`: {body}"))?;
    usize::try_from(total).context("list total does not fit a usize")
}

/// The offset to request next, or `None` once the rows already gathered
/// cover `total`. The pure pagination decision behind `list_all_pages`.
///
/// A limit is not a filter (memory: a-limit-is-not-a-filter). The job
/// list is `ORDER BY opened_on DESC`, so a car opened days ago but
/// gated and parked today has an OLD `opened_on` and sorts to the tail.
/// Once more than one page of open cars exist (in-flight + parked +
/// landed-but-unclosed residue), a parked-ready car falls off page one
/// and, read with a bare `limit=`, never boards — silently, and exactly
/// when a backlog builds. Looping on this until it returns `None` makes
/// a read see every matching row.
pub(crate) fn next_offset(total: usize, fetched: usize) -> Option<usize> {
    if fetched >= total {
        None
    } else {
        Some(fetched)
    }
}

/// Every row of a paginated `/api/jobs` list, not just page one.
///
/// `fetch` is handed the offset to request and returns that page's body
/// (with `data` and `total`); this pages on `offset` — via
/// [`next_offset`] over the response's [`list_total`] — until the rows
/// gathered cover `total`. Shared by every whole-open-set read in the
/// conductor: boarding (`candidates`), the branch-sweep guard
/// (`open_car_branches`), the dock's merge preview (`preview_dock`),
/// and the cadence loop's dock-depth probe (`cadence::probe_dock_depth`)
/// — one paginator so none of them can under-read the dock again.
///
/// Terminates: each page advances `offset` by the rows it returned, and
/// a page that returns nothing stops the loop, so a `total` that shrinks
/// mid-read (a car closing between pages) cannot spin it.
pub(crate) async fn list_all_pages<F, Fut>(fetch: F) -> Result<Vec<Value>>
where
    F: Fn(usize) -> Fut,
    Fut: std::future::Future<Output = Result<Option<Value>>>,
{
    let mut out: Vec<Value> = Vec::new();
    loop {
        let body = fetch(out.len())
            .await?
            .ok_or_else(|| anyhow!("empty response for a list call"))?;
        let total = list_total(&body)?;
        let page = rows(Some(body))?;
        if page.is_empty() {
            break;
        }
        out.extend(page);
        if next_offset(total, out.len()).is_none() {
            break;
        }
    }
    Ok(out)
}

pub(crate) fn find_step<'a>(job: &'a Value, slug: &str, title: &str) -> Option<&'a Value> {
    // One lookup, defined in core beside the car builders: the parkers
    // read the same review step this file boards (CLAUDE.md 9a).
    boss_jobs::car::find_step(job, slug, title)
}

pub(crate) fn step_done(step: Option<&Value>) -> bool {
    step.and_then(|s| s.get("status"))
        .and_then(Value::as_str)
        .is_some_and(|s| s == "completed" || s == "skipped")
}

/// `spec_slug or title` — the label the python conductor logged.
fn step_label(step: &Value) -> String {
    step.get("spec_slug")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .or_else(|| step.get("title").and_then(Value::as_str))
        .unwrap_or("?")
        .to_string()
}

pub(crate) fn id8(id: &str) -> String {
    id.chars().take(8).collect()
}

fn job_id(job: &Value) -> Result<&str> {
    job.get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("job without an id"))
}

/// Python truthiness for the metadata fields the conductor reads —
/// absent, null, "", 0 and empty containers are all "not set".
pub(crate) fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

/// How long a gate-run may stay active before it is presumed dead: the
/// gate Job's own `activeDeadlineSeconds` (10800 = 3h) from
/// `infra/gate-runner/gate-runner.yaml`. Past it Kubernetes has killed
/// the Job. CLAUDE.md §9a — the manifest is the authority; if that
/// deadline moves, move this. A CEILING, not an expectation (a normal
/// gate finishes in ~15-90 min), so it can only settle runs truly gone.
pub(crate) const GATE_DEADLINE_HOURS: i64 = 3;

/// How long a gate-run has been active with no verdict, when that is
/// long enough to call it dead — `None` means leave it alone.
///
/// Pure so the decision is testable without an API: a run is dead when
/// it has NOT reported a verdict AND its `opened_at` predates the Job
/// deadline. A run with no `opened_at` yields `None` — absence of a
/// stamp is not evidence of death, and settling on a guess would put a
/// verdict nobody observed into the audit log.
pub(crate) fn dead_gate_run_hours(run: &Value, now: DateTime<Utc>) -> Option<i64> {
    let verdict_step = find_step(run, "record-verdict", "Record the gate verdict");
    if step_done(verdict_step) {
        return None;
    }
    let opened = metadata_map(run)
        .get("opened_at")
        .and_then(Value::as_str)
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|t| t.with_timezone(&Utc))?;
    let hours = (now - opened).num_hours();
    (hours >= GATE_DEADLINE_HOURS).then_some(hours)
}

/// Should this closed gate-run's verdict be buried if its sha landed?
/// Returns the (sha, verdict) to check when the run is closed with a
/// `failed` or `lost` verdict, names a sha, is not already superseded,
/// and was opened within the yard's approach window — the same two days
/// after which the lens calls a closed gate archaeology, so nothing
/// older is touched. Everything else is None: a green needs no burial,
/// an open run is live activity, and an annotated run is settled.
pub(crate) const APPROACH_FRESH_DAYS: i64 = 2;

pub(crate) fn verdict_to_bury(run: &Value, now: DateTime<Utc>) -> Option<(String, String)> {
    if run.get("status").and_then(Value::as_str) != Some("closed") {
        return None;
    }
    let md = metadata_map(run);
    if md
        .get("superseded")
        .is_some_and(|v| !v.is_null() && v != &Value::Bool(false))
    {
        return None;
    }
    let verdict_step = find_step(run, "record-verdict", "Record the gate verdict");
    let verdict = verdict_step
        .and_then(|s| s.get("metadata"))
        .and_then(|m| m.get("verdict"))
        .and_then(Value::as_str)
        .or_else(|| md.get("outcome").and_then(Value::as_str))?;
    if verdict != "failed" && verdict != "lost" {
        return None;
    }
    let sha = md.get("sha").and_then(Value::as_str)?.to_string();
    if sha.is_empty() {
        return None;
    }
    let opened = md
        .get("opened_at")
        .and_then(Value::as_str)
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|t| t.with_timezone(&Utc))?;
    if (now - opened).num_days() > APPROACH_FRESH_DAYS {
        return None;
    }
    Some((sha, verdict.to_string()))
}

/// Why a green with no car is worth an alarm — which decides both the
/// window it is judged against and what the packet says failed
/// (CLAUDE.md §Diagnosis: "a verdict must name what failed").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StrandCause {
    /// The gate carried a `--park-*` intent, so `jobs.auto-park` owed
    /// this green a car and recorded neither a car nor a `park_skipped`
    /// decision. The handler failed; the operator does not need to wait
    /// out a threshold to be told so.
    AutoParkFailed,
    /// A human gated without a park intent and never parked it. Nothing
    /// is broken — it is forgotten — so it is judged on elapsed time.
    NeverParked,
}

impl StrandCause {
    fn from_intent(park_intent: bool) -> Self {
        if park_intent {
            Self::AutoParkFailed
        } else {
            Self::NeverParked
        }
    }
}

/// A stranded green worth an alarm: a gate-run that went GREEN, whose
/// branch no car ever claimed, that has sat that way past the window
/// its cause is judged on, and that no open alarm already names.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct StrandedGreen {
    pub branch: String,
    pub gate_run_id: String,
    pub age_mins: i64,
    pub cause: StrandCause,
}

/// The two windows this alarm runs on, and why there are two.
///
/// MEASURED 2026-09-09 against the system of record, over the 12 most
/// recent green gate-runs that carried a park intent: 11 had their car
/// filed 0.6 SECONDS after the verdict — `jobs.auto-park` fires on the
/// `step.done.gate-verdict` event, so the green-to-parked path is
/// sub-second, not minutes. A single 45-minute threshold was therefore
/// not "the ordinary gap plus margin"; it was a wait for something that
/// either happened instantly or was never going to happen.
#[derive(Debug, Clone, Copy)]
pub(crate) struct StrandWindows {
    /// For [`StrandCause::NeverParked`] — a human's forgotten green.
    /// Elapsed time is the only signal there is, so this stays a
    /// threshold, now measured from the VERDICT rather than from the
    /// gate's start (see [`freshest_green`]).
    pub never_parked_mins: i64,
    /// For [`StrandCause::AutoParkFailed`] — grace for the handler,
    /// not a wait for the happy path: sized to cover a dispatcher
    /// restart or a redelivery, ~1000x the measured 0.6s latency.
    pub auto_park_grace_mins: i64,
}

impl StrandWindows {
    fn for_cause(&self, cause: StrandCause) -> i64 {
        match cause {
            StrandCause::AutoParkFailed => self.auto_park_grace_mins,
            StrandCause::NeverParked => self.never_parked_mins,
        }
    }
}

/// The FRESHEST green gate-run for a branch: its id, how long ago it
/// went green, and whether it carried a park intent.
///
/// AGE BASIS, in preference order — every one of them dates the
/// VERDICT, never the gate's start:
///   1. the verdict step's own `completed_at` column, which the jobs
///      API stamps on every completion (`automation:gate-runner`);
///   2. the same key inside the step's `metadata`, honoured for free in
///      case a runner ever writes one there;
///   3. the run's `metadata.closed_at` — the outcome rule closes the
///      packet ~0.2s after the verdict lands.
///
/// `opened_at` IS NOT A BASIS, and using it is the defect this
/// replaces. A gate-run opens BEFORE it goes green, so dating the green
/// from the open over-states its age by the whole gate duration —
/// measured 2026-09-09 over the 21 most recent green runs: median 18.9
/// min, max 26.1 min, and a run that waits for a concurrency slot
/// (`queued_at`) can be much longer. Against a 45-minute threshold that
/// left an effective post-green window of 45 − duration, sometimes
/// ZERO: the alarm could fire on a green the instant the verdict landed,
/// ahead of the auto-park handler it was supposedly waiting for. Four
/// false STRANDED GREEN packets on 2026-09-09 (e60398dc) are what that
/// looks like from the queue.
///
/// No dateable green on ANY of the branch's runs → `None`, and the
/// branch is left for a later pass rather than alarmed on a guessed age
/// (the `dead_gate_run_hours` idiom: absence of a stamp is not
/// evidence).
fn freshest_green(
    gate_runs: &[Value],
    branch: &str,
    now: DateTime<Utc>,
) -> Option<(String, i64, bool)> {
    let mut best: Option<(String, DateTime<Utc>, bool)> = None;
    for g in gate_runs {
        let md = metadata_map(g);
        if md.get("branch").and_then(Value::as_str) != Some(branch) {
            continue;
        }
        // The same green flag the shared definition keys on: any step
        // whose metadata.verdict is green.
        let green_step = g
            .get("steps")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .find(|s| {
                s.get("metadata")
                    .and_then(|m| m.get("verdict"))
                    .and_then(Value::as_str)
                    == Some(boss_jobs::stranded::VERDICT_GREEN)
            });
        let Some(green_step) = green_step else {
            continue;
        };
        let Some(at) = green_step
            .get("completed_at")
            .and_then(Value::as_str)
            .or_else(|| {
                green_step
                    .get("metadata")
                    .and_then(|m| m.get("completed_at"))
                    .and_then(Value::as_str)
            })
            .or_else(|| md.get("closed_at").and_then(Value::as_str))
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|t| t.with_timezone(&Utc))
        else {
            continue;
        };
        let id = g
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let intent = boss_jobs::stranded::park_intent(g.get("metadata").unwrap_or(&Value::Null));
        match &best {
            Some((_, best_at, _)) if *best_at >= at => {}
            _ => best = Some((id, at, intent)),
        }
    }
    best.map(|(id, at, intent)| (id, (now - at).num_minutes(), intent))
}

/// Which stranded greens are past their window AND not already
/// alarmed — the pure decision the reconcile alarm rides on. Detection
/// (green + no car + no spent marker + not held) is
/// `census::stranded_gate_runs`, which reads `boss_jobs::stranded`, the
/// ONE definition the yard read-model uses too (CLAUDE.md §9a); this
/// layers the cause-specific window (a just-gated green is not stranded
/// yet; a green whose auto-park never ran is stranded almost at once)
/// and the dedup (`already_alarmed` is the branches an open alarm
/// already names, so a persisting strand is ONE packet, not one every
/// ten minutes).
pub(crate) fn stranded_greens_to_alarm(
    gate_runs: &[Value],
    car_branches: &BTreeSet<String>,
    already_alarmed: &BTreeSet<String>,
    now: DateTime<Utc>,
    windows: StrandWindows,
) -> Vec<StrandedGreen> {
    let mut out: Vec<StrandedGreen> = Vec::new();
    for branch in crate::census::stranded_gate_runs(gate_runs, car_branches) {
        if already_alarmed.contains(&branch) {
            continue;
        }
        let Some((gate_run_id, age_mins, park_intent)) = freshest_green(gate_runs, &branch, now)
        else {
            continue;
        };
        let cause = StrandCause::from_intent(park_intent);
        if age_mins < windows.for_cause(cause) {
            continue;
        }
        out.push(StrandedGreen {
            branch,
            gate_run_id,
            age_mins,
            cause,
        });
    }
    out.sort_by(|a, b| a.branch.cmp(&b.branch));
    out
}

/// The stamp this alarm leaves when IT closes one of its own packets,
/// so a reader (and any future settled-suppression) can tell a machine
/// clear from a human's answer. The silence sweep's `cleared_by`
/// idiom, verbatim.
pub(crate) const STRANDED_CLEARED_BY: &str = "conductor.stranded-green-alarm";

/// Open alarms whose claim no longer holds: the branch parked, landed,
/// was re-railed, was held, or was superseded, so it is no longer in
/// `stranded_now`. Returns `(alarm id, branch)`.
///
/// THIS IS THE HALF THAT WAS MISSING. The alarm raised and nothing ever
/// revisited the claim, so a branch that parked a minute later left a
/// permanent false packet on the operator's queue — four of them on
/// 2026-09-09, every one closed by hand (e60398dc). An alarm that
/// cannot clear itself is a claim the system stops standing behind the
/// instant it stops being true.
pub(crate) fn stranded_alarms_to_clear(
    open_alarms: &[Value],
    stranded_now: &BTreeSet<String>,
) -> Vec<(String, String)> {
    open_alarms
        .iter()
        .filter_map(|j| {
            let branch = j
                .get("metadata")
                .and_then(|m| m.get("stranded_branch"))
                .and_then(Value::as_str)?;
            if stranded_now.contains(branch) {
                return None;
            }
            let id = j.get("id").and_then(Value::as_str)?;
            Some((id.to_string(), branch.to_string()))
        })
        .collect()
}

/// WHY a strand ended, named from the same data the detection reads —
/// so the clear says what happened rather than "no longer stranded".
pub(crate) fn stranded_clear_reason(
    gate_runs: &[Value],
    car_branches: &BTreeSet<String>,
    branch: &str,
) -> String {
    if car_branches.contains(branch) {
        return format!("a ship-a-change car now carries `{branch}`");
    }
    let run = gate_runs.iter().find(|g| {
        g.get("metadata")
            .and_then(|m| m.get("branch"))
            .and_then(Value::as_str)
            == Some(branch)
    });
    let md = run.and_then(|g| g.get("metadata")).unwrap_or(&Value::Null);
    if let Some(marker) = boss_jobs::stranded::spent_reason(md) {
        return format!("its gate-run is marked `{marker}` — the green is spent, not waiting");
    }
    if let Some(reason) = boss_jobs::stranded::hold_reason(md) {
        return format!("its gate-run is HELD on purpose: {reason}");
    }
    format!("no green gate-run for `{branch}` is unclaimed any more")
}

/// The measurement a STANDING alarm is refreshed with instead of being
/// twinned — `PATCH /api/jobs/{id}/metadata` merges top-level keys, so
/// this is exactly the fields that move between passes.
pub(crate) fn stranded_refresh_patch(a: &StrandedGreen, now: DateTime<Utc>) -> Value {
    json!({
        "gate_run_id": a.gate_run_id,
        "verdict_age_mins": a.age_mins,
        "stranded_cause": cause_key(a.cause),
        "last_measured_at": now.to_rfc3339(),
    })
}

/// The triage completion that CLOSES a standing alarm when the branch
/// stops being stranded. `disposition = "stale"` is the backlog-item
/// terminal titled "Closed — the claim no longer holds", which is
/// exactly the case. PUT on a step REPLACES top-level metadata, so the
/// step's existing keys are carried through.
pub(crate) fn stranded_clear_step_body(
    existing: &Map<String, Value>,
    branch: &str,
    why: &str,
) -> Value {
    let mut metadata = existing.clone();
    metadata.insert("disposition".into(), json!("stale"));
    metadata.insert(
        "evidence".into(),
        json!(format!(
            "The conductor re-measured `{branch}` and it is no longer a stranded green: \
             {why}. The claim this alarm carried no longer holds; closed by machine, not \
             by judgement. A branch that strands again files a new packet."
        )),
    );
    metadata.insert("cleared_by".into(), json!(STRANDED_CLEARED_BY));
    json!({"status": "completed", "metadata": metadata})
}

fn cause_key(cause: StrandCause) -> &'static str {
    match cause {
        StrandCause::AutoParkFailed => "auto-park-failed",
        StrandCause::NeverParked => "never-parked",
    }
}

/// The backlog-item an unrescued stranded green becomes. Mirrors
/// `estate.alarm`'s raise (a5adfb99): a `backlog-item` on the
/// operator's queue the overdue/watchlist machinery can see, with the
/// dedup key in metadata. The key is `stranded_branch` — the branch,
/// stable across a re-gate (a new gate-run id would let the same strand
/// re-alarm; the branch will not). Priority is `standard`, not
/// `urgent`: a strand decays over days, not minutes, and an alarm that
/// cries urgent over non-urgent things trains operators to ignore it
/// (estate.alarm's own calibration lesson). The packet NAMES ITS CAUSE:
/// an auto-park that never filed is a broken actor, and a human's
/// forgotten green is not, and they are not rescued the same way.
pub(crate) fn stranded_alarm_body(
    a: &StrandedGreen,
    windows: StrandWindows,
    now: DateTime<Utc>,
) -> Value {
    let window = windows.for_cause(a.cause);
    let title = match a.cause {
        StrandCause::AutoParkFailed => format!(
            "STRANDED GREEN: {} gated green {}min ago and auto-park filed no car",
            a.branch, a.age_mins
        ),
        StrandCause::NeverParked => format!(
            "STRANDED GREEN: {} gated green {}min ago, never parked",
            a.branch, a.age_mins
        ),
    };
    let cause_detail = match a.cause {
        StrandCause::AutoParkFailed => format!(
            "This green CARRIED a `--park-*` intent, so `jobs.auto-park` owed it a car and \
             recorded neither a car nor a `park_skipped` decision within {window}min — \
             measured 2026-09-09, the handler files in 0.6 SECONDS when it runs, so this is \
             the handler having failed, not a slow happy path. Check the dispatcher: the \
             rule on `step.done.gate-verdict`, and the handler's journal for this gate-run."
        ),
        StrandCause::NeverParked => format!(
            "This green carried NO park intent — a hand gate that was never parked — and has \
             sat that way for {}min (window {window}min).",
            a.age_mins
        ),
    };
    json!({
        "kind": "backlog-item",
        "status": "open",
        "title": title,
        "subject": {"subject_kind": "custom", "id": "bosspipeline"},
        "owner_id": "emp-david",
        "priority": "standard",
        "opened_on": now.date_naive().to_string(),
        "tags": ["pipeline", "gate"],
        "metadata": {
            "area": "pipeline",
            "stranded_branch": a.branch,
            "gate_run_id": a.gate_run_id,
            "verdict_age_mins": a.age_mins,
            "stranded_cause": cause_key(a.cause),
            "last_measured_at": now.to_rfc3339(),
            "detail": format!(
                "Gate-run {} for `{}` went GREEN {}min ago but no ship-a-change car ever \
                 claimed the branch: it gated, was never parked, so it never reached the \
                 dock and cannot board. {} A stranded green DECAYS: gated yesterday, \
                 unmergeable today, and a later blind rescue reverts landed work (the decay \
                 jobs.auto-park was built to end, 2026-09-01). RESCUE = rebase the branch \
                 onto current origin/main + re-gate (its base has likely moved); never \
                 rebuild blind. If the change already landed via another branch, close this \
                 stale. The age is measured from the VERDICT, not from the gate's start. \
                 This packet CLOSES ITSELF when the branch parks, lands, is held or is \
                 re-railed — if it is still open, the claim still holds.",
                id8(&a.gate_run_id),
                a.branch,
                a.age_mins,
                cause_detail,
            ),
        },
    })
}

pub(crate) fn metadata_map(v: &Value) -> Map<String, Value> {
    match v.get("metadata") {
        Some(Value::Object(m)) => m.clone(),
        _ => Map::new(),
    }
}

/// The overlay half of `merge_job_metadata`, pure: jobs-api's PATCH
/// semantics stop at the top level — a PUT replaces `metadata`
/// wholesale — so every update must carry the record's existing keys
/// forward. A `Value::Null` value REMOVES the key: how a boarding car
/// sheds a stale `skip_reason` instead of carrying "" forever.
/// Has this train's arrival report already been filed?
///
/// Reads the JOB's metadata, not the `arrived` step's. The report moved
/// there when terminal steps became immutable (f402a681) — and the
/// idempotence check has to move with it, or every reconcile re-files a
/// report it already wrote. That is the failure mode the 2026-08-13
/// journal records as "re-file its arrival report" making the conductor
/// look broken while the trains had in fact landed.
pub(crate) fn arrival_already_filed(train: &Value) -> bool {
    train
        .get("metadata")
        .and_then(|m| m.get("arrival_report"))
        .is_some_and(|v| !v.is_null())
}

pub(crate) fn overlay_metadata(container: &Value, kv: Vec<(&str, Value)>) -> Map<String, Value> {
    let mut md = metadata_map(container);
    for (k, v) in kv {
        match v {
            Value::Null => {
                md.remove(k);
            }
            v => {
                md.insert(k.to_string(), v);
            }
        }
    }
    md
}

/// The skip reason for a car whose branch would not merge onto this
/// window's train: names the conflicted files, truncated to stay
/// chip-sized. At least one file always shows.
/// Replay `branch`'s own commits on top of the consist as it stands,
/// returning a ref that merges cleanly — or `None` when the car has a
/// conflict a rebase cannot resolve either.
///
/// Rebases from the merge-base, so only the car's OWN work is replayed:
/// anything it carries that already reached main (the squash-merge
/// case) is dropped by git as an applied patch rather than re-applied
/// as a conflict.
///
/// Leaves the clone on `train_branch` whatever happens — a caller
/// mid-consist must not be handed a detached HEAD or a half-finished
/// rebase, and the next car in the loop merges into whatever branch it
/// finds itself on.
fn rerail_onto_consist(clone: &str, train_branch: &str, branch: &str) -> Result<Option<String>> {
    let scratch = "boss-train-rerail";
    let car = format!("fork/{branch}");
    let base_out = sh_unchecked(&["git", "-C", clone, "merge-base", train_branch, &car])?;
    if !base_out.status.success() {
        return Ok(None);
    }
    let base = stdout_str(&base_out).trim().to_string();
    if base.is_empty() {
        return Ok(None);
    }
    sh_unchecked(&["git", "-C", clone, "checkout", "-q", "-B", scratch, &car])?;
    let rebase = sh_unchecked(&[
        "git",
        "-C",
        clone,
        "rebase",
        "--onto",
        train_branch,
        &base,
        scratch,
    ])?;
    if !rebase.status.success() {
        sh_unchecked(&["git", "-C", clone, "rebase", "--abort"])?;
        sh_unchecked(&["git", "-C", clone, "checkout", "-q", train_branch])?;
        return Ok(None);
    }
    sh_unchecked(&["git", "-C", clone, "checkout", "-q", train_branch])?;
    Ok(Some(scratch.to_string()))
}

pub(crate) fn skip_reason_conflict(conflicted: &[String], file_budget: usize) -> String {
    if conflicted.is_empty() {
        return "conflict: unresolved (merge died before conflict markers)".to_string();
    }
    let mut shown = 0usize;
    let mut len = 0usize;
    for f in conflicted {
        let add = f.len() + if shown == 0 { 0 } else { 2 };
        if shown > 0 && len + add > file_budget {
            break;
        }
        shown += 1;
        len += add;
    }
    let files = conflicted[..shown].join(", ");
    match conflicted.len() - shown {
        0 => format!("conflict: {files}"),
        hidden => format!("conflict: {files} +{hidden} more"),
    }
}

/// Put a car's branch on the fork when the conductor can already see
/// it, and say whether it did.
///
/// TWO SOURCES, TRIED IN ORDER, and both are refs the author already
/// published — copying one to the fork is not a judgement call.
///
/// 1. `origin/<branch>`. The natural place to push is the upstream you
///    cloned; the fork is an implementation detail of how this
///    conductor assembles a train. On 2026-08-14 that gap silently
///    held NINE cars for a session — the dock reported 12 parked while
///    the boardable count was 0, because `parked_ready` asks "branch
///    declared, review ready" and boarding asks "branch on the fork".
///
/// 2. `refs/heads/<branch>` — a LOCAL ref in the conductor's own
///    clone. This is what `git push gcp <branch>` produces, because
///    the `gcp` remote IS /var/lib/boss-train/repo. The ref lands here
///    on no remote, and the car was skipped while its branch sat in
///    the conductor's working copy. On 2026-08-16 that cost five
///    hand-run pushes in one evening: a human ran `git push origin`
///    from this very directory, with the credentials the conductor
///    already holds. The old comment called a branch in neither place
///    "never pushed at all", which stopped being true the moment
///    anyone could push to this clone directly.
///
/// Returns `Ok(false)` when neither ref exists — that car really was
/// never pushed, and skipping it is correct. Never called in dry mode.
///
/// WHICH REF WINS WHEN BOTH EXIST: the DESCENDANT, never "whichever
/// was listed first". Until 2026-08-17 this returned on the first ref
/// that pushed successfully, with `origin/<branch>` listed first, so a
/// stale remote-tracking ref beat the car's real head. Two trains in a
/// row assembled code their author had already fixed
/// (`feat/presence-assurance` origin=58083e8 vs local=6cfc15e;
/// `feat/estate-subjects` origin=0381fe6 vs local=d00307b, eight
/// commits behind) and both failed CI on those exact fixed errors —
/// silently, while reporting success. Packet `9150dc6b`.
///
/// Reordering the list is NOT sufficient, and the second test below
/// is the reason: this function is only ever called when the fork
/// does not have the branch, so *any* source pushes cleanly and
/// nothing rejects a stale one. Fast-forward rejection cannot be
/// leaned on as the tiebreak — the choice has to be made here.
///
/// Both remotes are the same forge in production, so "ahead" is the
/// only thing that distinguishes the refs. Unrelated histories cannot
/// be ordered, so the local head wins as the more recent statement of
/// intent in this clone.
pub(crate) fn publish_car_branch(clone: &str, branch: &str) -> Result<bool> {
    let exists = |src: &str| -> Result<bool> {
        Ok(
            sh_unchecked(&["git", "-C", clone, "rev-parse", "--verify", "--quiet", src])?
                .status
                .success(),
        )
    };
    // `<a> is an ancestor of <b>`, i.e. b is at or ahead of a.
    let is_ancestor = |a: &str, b: &str| -> Result<bool> {
        Ok(
            sh_unchecked(&["git", "-C", clone, "merge-base", "--is-ancestor", a, b])?
                .status
                .success(),
        )
    };

    let local = format!("refs/heads/{branch}");
    let upstream = format!("origin/{branch}");
    let (have_local, have_upstream) = (exists(&local)?, exists(&upstream)?);

    // THE FORGE IS THE AUTHORITY ON A CAR'S BRANCH. Read its current head
    // before deciding anything: a clone's remote-tracking ref is a memory
    // of an earlier fetch, and on 2026-09-05 a restacked car's forge
    // branch read as the PRE-restack sha after a boarding (packet
    // c1d56d13). If the forge already carries what this clone would
    // push — or has moved beyond it — there is nothing to publish, and
    // pushing a stale copy is exactly the overwrite this guards against.
    // The push below is a plain push (never --force), so git would refuse
    // a diverged head anyway; refusing here says WHY in the journal
    // instead of a silent rc=1.
    // A `+` refspec: the tracking ref may hold an OLDER head (the very
    // memory this guards against), and a plain fetch refuses to move it
    // backwards-and-sideways — which would leave the clone believing the
    // fork still carries what it last saw.
    let _ = sh_unchecked(&[
        "git",
        "-C",
        clone,
        "fetch",
        "-q",
        "fork",
        &format!("+refs/heads/{branch}:refs/remotes/fork/{branch}"),
    ]);
    let fork_ref = format!("fork/{branch}");
    if exists(&fork_ref)? {
        let candidate = match (have_local, have_upstream) {
            (true, _) => Some(local.clone()),
            (false, true) => Some(upstream.clone()),
            (false, false) => None,
        };
        if let Some(c) = candidate {
            let same = is_ancestor(&c, &fork_ref)? && is_ancestor(&fork_ref, &c)?;
            if same {
                // Already published: the fork carries exactly this head.
                // Nothing to push, and the answer is "yes, it is there" —
                // publishing twice is idempotent, not a refusal.
                return Ok(true);
            }
            let fork_ahead = is_ancestor(&c, &fork_ref)?;
            let diverged = !fork_ahead && !is_ancestor(&fork_ref, &c)?;
            if fork_ahead || diverged {
                let fork_head =
                    sh_unchecked(&["git", "-C", clone, "rev-parse", "--short", &fork_ref])?;
                log(format!(
                    "{branch}: the fork already carries {} ({}) — not publishing {c} over it",
                    stdout_str(&fork_head).trim(),
                    if fork_ahead {
                        "ahead of this clone"
                    } else {
                        "diverged from this clone"
                    }
                ));
                return Ok(false);
            }
        }
    }

    let src = match (have_local, have_upstream) {
        (false, false) => return Ok(false),
        (true, false) => local,
        (false, true) => upstream,
        // Prefer the local head unless upstream is strictly ahead of
        // it. Equal refs take the local branch, which is the same
        // commit by definition.
        (true, true) => {
            if is_ancestor(&local, &upstream)? && !is_ancestor(&upstream, &local)? {
                upstream
            } else {
                local
            }
        }
    };

    let pushed = sh_unchecked(&[
        "git",
        "-C",
        clone,
        "push",
        "fork",
        &format!("{src}:refs/heads/{branch}"),
    ])?;
    if !pushed.status.success() {
        return Ok(false);
    }
    // Name the ref and the sha that actually shipped. The 2026-08-17
    // diagnosis cost a second red train precisely because publishing
    // said nothing about WHAT it published.
    let sha = sh_unchecked(&["git", "-C", clone, "rev-parse", "--short", &src])?;
    log(format!(
        "published {branch} to the fork from {src} @ {}",
        String::from_utf8_lossy(&sha.stdout).trim()
    ));
    Ok(true)
}

/// The sha `publish_car_branch` would ship for this branch, or `None`
/// when the conductor can see no ref for it at all.
///
/// Exists so BOARDING can ask the same question PUBLISHING answers.
/// Until 2026-08-17 `candidates` only asked "is this branch on the
/// fork", which is not the question: once a branch is on the forge, a
/// later commit to it never gets there, so a car repaired after a red
/// train boards the version that just failed. It happened twice in one
/// day — `feat/dev-shared-target` was `3370b42` locally and `96109f7`
/// on the forge, and train 38d49597 assembled the red one. Packet
/// `7d2f30b9`.
pub(crate) fn car_head(clone: &str, branch: &str) -> Result<Option<String>> {
    for src in [format!("refs/heads/{branch}"), format!("origin/{branch}")] {
        let out = sh_unchecked(&["git", "-C", clone, "rev-parse", "--verify", "--quiet", &src])?;
        if out.status.success() {
            let sha = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !sha.is_empty() {
                // Whichever of the two publish_car_branch would pick
                // resolves to a commit; comparing the LOCAL one first
                // is enough to notice a fork that has fallen behind,
                // and publish_car_branch still makes the final choice.
                return Ok(Some(sha));
            }
        }
    }
    Ok(None)
}

/// The head this car will actually board: the fork ref the consist is
/// assembled from.
///
/// `car_head` answers a DIFFERENT question — "is there a newer commit
/// anywhere that publishing should ship" — and prefers the conductor
/// clone's own `refs/heads` to answer it. That is right for deciding
/// whether to publish and wrong as an answer to "what will ride", and
/// the two come apart whenever a car is rebased and re-pushed. The
/// clone keeps the pre-rebase commit; `publish_car_branch` cannot
/// fast-forward the fork past it, so the fork rightly keeps the gated
/// commit and boards it. `rerail_onto_consist` reads `fork/{branch}`
/// and the boarded head is stamped from it, so this is the only ref a
/// gate receipt can honestly be checked against.
///
/// Measured on a live dock, 2026-08-29: car c6531868 was left behind
/// for "gated, then changed" while its receipt (56b817eb) matched the
/// fork exactly — the mismatch was against a local ref eight hours
/// older that no train would ever have carried.
pub(crate) fn fork_head(clone: &str, branch: &str) -> Result<Option<String>> {
    let out = sh_unchecked(&[
        "git",
        "-C",
        clone,
        "rev-parse",
        "--verify",
        "--quiet",
        &format!("fork/{branch}"),
    ])?;
    if !out.status.success() {
        return Ok(None);
    }
    let sha = String::from_utf8_lossy(&out.stdout).trim().to_string();
    Ok((!sha.is_empty()).then_some(sha))
}

/// The skip reason for a car parked at review whose branch was never
/// pushed to the fork.
pub(crate) fn skip_reason_branch_missing(branch: &str) -> String {
    format!("branch {branch} not on fork")
}

/// Which of a repo's action runs belong to this train and are still
/// burning the runner — the decision half of "cancelling a train
/// should cancel its CI", kept pure so it can be tested against real
/// API shapes rather than a fake.
///
/// MEASURED FIELD SHAPES, 2026-08-17, against this Forgejo. The
/// obvious key does not work: **`head_branch` is `null` on every run**
/// this deployment returns, so a filter written against it cancels
/// nothing at all, silently, which is indistinguishable from "there
/// was nothing to cancel". Two fields ARE populated and identify a
/// train's runs:
///   - `prettyref`  — `"#64"` for a pull_request run, i.e. the PR the
///     conductor already holds the url of;
///   - `commit_sha` — the train branch head the run was queued for.
/// Either is sufficient; both are matched so a run queued before the
/// PR existed is still caught.
///
/// CONSERVATIVE ON STATUS, deliberately. Only runs in a known-active
/// state are cancelled. The alternative — "anything not in a terminal
/// set" — cancels runs whose status this code has never heard of, and
/// the cost of a false cancel (killing someone's live run) is much
/// higher than the cost of a miss (the run finishes and wastes the
/// time it was already wasting).
pub(crate) fn cancellable_run_ids(runs: &[Value], pr_index: &str, head_sha: &str) -> Vec<i64> {
    const ACTIVE: [&str; 3] = ["running", "waiting", "blocked"];
    let want_ref = format!("#{pr_index}");
    runs.iter()
        .filter(|r| {
            let status = r.get("status").and_then(Value::as_str).unwrap_or_default();
            ACTIVE.contains(&status)
        })
        .filter(|r| {
            let pretty = r
                .get("prettyref")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let sha = r
                .get("commit_sha")
                .and_then(Value::as_str)
                .unwrap_or_default();
            (!pr_index.is_empty() && pretty == want_ref)
                || (!head_sha.is_empty() && !sha.is_empty() && sha == head_sha)
        })
        .filter_map(|r| r.get("id").and_then(Value::as_i64))
        .collect()
}

/// Is this ship-a-change Job a parked ready car — at review with a
/// branch declared and not already on a train? ONE definition, shared
/// by the boarding collector below and the cadence loop's dock-depth
/// probe (`boss train cadence`, the queue-depth basis): the count that
/// fires a boarding must be the same predicate boarding itself uses.
/// (The fork-branch existence check stays in `candidates` — it needs
/// the clone, and a car whose branch was never pushed still occupies
/// the dock from the author's point of view.)
pub(crate) fn parked_ready(job: &Value) -> bool {
    // "Parked" — named branch, no train stamp, review still waiting — is
    // core's predicate (`car::is_parked`), shared with `boss park` and
    // the auto-park handler so that what they refresh on a re-gate is
    // exactly what this counts at the dock. Boarding's own refinements
    // stay here: a `train/` branch is a consist, not a car, and a held
    // car is parked but must not ride.
    let md = job.get("metadata").cloned().unwrap_or(Value::Null);
    let branch = md.get("branch").and_then(Value::as_str).unwrap_or_default();
    if branch.starts_with("train/") || !boss_jobs::car::is_parked(job) {
        return false;
    }
    let review = find_step(job, "review", "Open for review");
    // A HELD car does not board. `metadata.hold` on the review step is
    // set by whoever parked it, and says "this is gated green and still
    // must not ride yet" - a car whose branch is correct but whose
    // WORLD is not. The case that forced this: a car repointing the
    // gate rig at a node label was parked green, and hours later the
    // only node carrying that label was cordoned for a hardware fault.
    // Landing it would have left the rig unschedulable and stopped
    // gating entirely. A note was written on the review step and the
    // conductor could not read it, because this predicate asked only
    // about status - documentation standing where a mechanism belonged.
    //
    // It lives HERE rather than in the boarding collector because the
    // cadence loop shares this predicate for dock depth: a held car
    // must not count toward the threshold either, or it would fire a
    // train it then declines to join, producing the empty windows that
    // made arrival rate unreadable (feedback f4baea39).
    //
    // THE MARKER READ IS NOT LOCAL. `stranded::hold_reason` is the one
    // definition of "is this marker set" — `null`/`false`/blank are no
    // marker, `true` is a marker with no reason — and the loading-dock
    // station row's `metadata_unmarked: ["hold"]` clause calls the same
    // function. This used to be a local `truthy`, which agreed by
    // coincidence rather than by construction; the station row had no
    // hold term at all until 20260910201453, and the two predicates
    // answering "is this car boardable" differently is the defect
    // backlog 36c3d4ca records (CLAUDE.md §9a).
    review
        .and_then(|s| s.get("metadata"))
        .and_then(boss_jobs::stranded::hold_reason)
        .is_none()
}

/// One branch a landed car's record makes a claim about, with
/// everything the sweep needs to act on it — so the decision and its
/// evidence travel together instead of the caller re-deriving the
/// evidence from a car id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CarBranch {
    /// The branch on the forge.
    pub(crate) branch: String,
    /// The car whose record proves this branch's content landed.
    pub(crate) car: String,
    /// The head the record says the branch pointed at when the train
    /// carried its content: the boarded head for the car's own branch,
    /// the head recorded at the rerail for a branch it was re-railed
    /// off. `None` = nothing recorded one, and the head guard refuses
    /// (`SweepGuard::NoRecord`) rather than guessing.
    pub(crate) head: Option<String>,
    /// A branch the car was re-railed OFF, rather than the one it
    /// boarded. Only the journal wording differs — the guards do not.
    pub(crate) rerail_origin: bool,
}

/// Every branch ONE car's record makes a landing claim about, in the
/// order the sweep should consider them: the branch it boarded first,
/// then each branch a rerail moved it off, oldest first. Empty unless
/// the car's own bookkeeping completed — closed with the `merged`
/// outcome (an abandoned car closes too, but its branch holds unmerged
/// work; abandonment is a disposition, not a sweep). `main` and
/// unnamed branches drop out here, and a branch named twice appears
/// once.
///
/// THE RERAIL ORIGINAL IS THE SECOND HALF (packet 473fda1b, generator
/// 1). `boss rerail` moves a car to a new branch and leaves the
/// original on the forge; the arriving train deletes the branch it
/// MERGED, which is the new one. The original was never a car, so a
/// sweep that iterates boarded cars had nothing to act on and would
/// never consider it — permanent by construction, and 13 branches deep
/// when it was measured on 2026-09-10. The link is read from the
/// provenance `boss rerail` RECORDS on the car (`rerail_origins`, each
/// entry naming a branch and the head it carried when the car left
/// it), never from a `-rerail` suffix guessed off a name: a name is not
/// a record, and the head is what the guard needs anyway.
fn recorded_branches(car: &Value) -> Vec<(String, Option<String>, bool)> {
    let md = car.get("metadata");
    let landed = car.get("status").and_then(Value::as_str) == Some("closed")
        && md.and_then(|m| m.get("outcome")).and_then(Value::as_str) == Some("merged");
    if !landed {
        return Vec::new();
    }
    let own = md
        .and_then(|m| m.get("branch"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let origins = md
        .and_then(|m| m.get("rerail_origins"))
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .map(|o| {
            (
                o.get("branch").and_then(Value::as_str).unwrap_or_default(),
                o.get("head")
                    .and_then(Value::as_str)
                    .filter(|h| !h.is_empty())
                    .map(str::to_string),
                true,
            )
        });
    let mut out: Vec<(String, Option<String>, bool)> = Vec::new();
    for (branch, head, origin) in
        std::iter::once((own, boarded_head(car).map(str::to_string), false)).chain(origins)
    {
        if branch.is_empty() || branch == "main" || out.iter().any(|(b, _, _)| b == branch) {
            continue;
        }
        out.push((branch.to_string(), head, origin));
    }
    out
}

/// Every branch of one car, as the sweep's decision record. A car with
/// no id cannot be named in a journal line, so it decides nothing.
fn car_branches(car: &Value) -> Vec<CarBranch> {
    let cid = car
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    if cid.is_empty() {
        return Vec::new();
    }
    recorded_branches(car)
        .into_iter()
        .map(|(branch, head, rerail_origin)| CarBranch {
            branch,
            car: cid.clone(),
            head,
            rerail_origin,
        })
        .collect()
}

/// The branch-sweep decision at arrival (protocol decision, David):
/// train PRs squash-merge, so git ancestry can never prove a car's
/// content landed — the JOB RECORD is the proof. Given the cars a
/// landed train boarded and the branches still-open cars name, a
/// branch is deletable iff:
///   - the car's own bookkeeping completed: closed with the `merged`
///     outcome (an abandoned car closes too, but its branch holds
///     unmerged work — never touch it);
///   - the branch is named and is not `main`;
///   - no still-open car rides the same branch (a follow-up car's
///     claim keeps it alive).
/// It holds for the branch the car BOARDED and for every branch a
/// rerail moved it off — one definition, because both are the same
/// question asked of the same record (see `recorded_branches`).
/// Two landed cars naming one branch delete it once. Pure — the
/// forge call and the journal line belong to the caller.
pub(crate) fn deletable_branches(
    boarded_cars: &[Value],
    open_branches: &BTreeSet<String>,
) -> Vec<CarBranch> {
    decided_branches(boarded_cars, |b| !open_branches.contains(b))
}

/// The branch head recorded when this car boarded — stamped by the
/// assembly onto the car Job in the same update that stamps `train`
/// (see `board`). Absent or empty reads as no stamp at all.
/// The gate-receipt spot-check (David's "Agreed" on 742d1faa): what
/// makes a car's green claim honest at the moment it matters — boarding.
/// `None` = the receipt vouches for exactly the head being boarded;
/// `Some(reason)` = leave the car behind, with the reason named on it.
///
/// Catches exactly the two lies that cost red trains: a receipt from a
/// different commit than the branch now points at (gate, then "one more
/// tiny fix" pushed after), and a receipt that was never green (or was
/// taken on a dirty tree, which is the same claim with extra steps —
/// the gate reads the tree live, so dirty means "green about something
/// else"). A car with NO receipt is unverifiable and stays behind too:
/// this check exists because claims without receipts already shipped.
pub(crate) fn receipt_skip_reason(car: &Value, boarding_head: Option<&str>) -> Option<String> {
    // A RE-GATE SUPERSEDES THE ORIGINAL GATE, and is read in preference
    // to it. Filed as user feedback 64cae7e9 after 17 of 34 left-behinds
    // traced to stale receipts: when a branch legitimately moves — a
    // migration renumbered off a collision, a rebase onto a main that had
    // moved into the same file — the receipt correctly stops vouching for
    // the head, and the car was then UNREPAIRABLE. Completed steps are
    // immutable, so the only recourse was to abandon the packet and park
    // a fresh one, losing the car's history and costing a packet every
    // time. That happened twice more on 2026-08-28, to cars 4e78035e and
    // 8b831c5c, which is what moved this from a filed opinion to a fix.
    //
    // IT LIVES IN JOB METADATA, NOT IN A NEW STEP, and that is a
    // deliberate retreat from the shape the feedback proposed. A `regate`
    // STEP was built and validated clean, then abandoned: `blocked_by` is
    // derived from every step a predicate REFERENCES, and a referenced
    // step that is merely pending makes the API refuse to complete the
    // referring step (the defect in feedback 1538e93a). Because a regate
    // step must key on `job.metadata.skip_reason` to appear only when the
    // conductor has left the car behind, and a predicate referencing
    // job.metadata never SKIPS, it would sit pending forever on every
    // healthy car — and anything referencing it, `review` included, would
    // be blocked from completing. That is the conductor unable to board
    // anything. The engine cannot express an optional repair step safely
    // today; job metadata can, so the repair uses what works and the
    // step is filed as protocol work behind 1538e93a.
    let regate = car
        .get("metadata")
        .and_then(|m| m.get("regate_receipt"))
        .filter(|v| !v.is_null());
    let md_owned;
    let md: &Value = match regate {
        Some(r) => {
            md_owned = json!({ "receipt": r });
            &md_owned
        }
        None => {
            let gate = find_step(car, "gate", "Green, and observed working")?;
            gate.get("metadata")?
        }
    };
    // Present as a JSON string (how the gate step records it) or as an
    // object (tooling that parses before writing) — both are receipts.
    let receipt: Value = match md.get("receipt") {
        Some(Value::String(s)) => serde_json::from_str(s).unwrap_or(Value::Null),
        Some(v @ Value::Object(_)) => v.clone(),
        _ => Value::Null,
    };
    if receipt.is_null() {
        return Some(
            "no machine receipt on the gate step — the green claim is unverifiable".into(),
        );
    }
    let verdict = receipt
        .get("verdict")
        .and_then(Value::as_str)
        .unwrap_or("?");
    if verdict != "green" {
        return Some(format!("gate receipt verdict is '{verdict}', not green"));
    }
    if receipt.get("dirty").and_then(Value::as_bool) == Some(true) {
        return Some(
            "gate receipt was taken on a dirty tree — it vouches for something else".into(),
        );
    }
    let receipt_head = receipt.get("head").and_then(Value::as_str).unwrap_or("");
    if receipt_head.is_empty() {
        return Some("gate receipt names no head — the green claim is unverifiable".into());
    }
    match boarding_head {
        Some(b) if commits_match(receipt_head, b) => None,
        Some(b) => Some(format!(
            "gate receipt is for {} but the branch boards {} — gated, then changed",
            &receipt_head[..receipt_head.len().min(8)],
            &b[..b.len().min(8)]
        )),
        // No boardable head resolved — the branch checks after this
        // will name that failure themselves; the receipt is not the
        // lie here.
        None => None,
    }
}

pub(crate) fn boarded_head(car: &Value) -> Option<&str> {
    car.get("metadata")?
        .get("boarded_head")?
        .as_str()
        .filter(|s| !s.is_empty())
}

/// The sweep's second question, and the answers to it.
///
/// `deletable_branches` asks whether the job record proves the car's
/// CONTENT landed. It does not — it cannot — prove the branch still
/// holds only that content. Car 23923b40's known_gap is what the gap
/// costs: `fix/conductor-hardening` boarded at fc55e4d, two more
/// commits were pushed to the branch AFTER boarding, the train landed
/// carrying only the boarded ones, and the sweep deleted the branch
/// on a job record that was entirely correct. The unmerged commits
/// went with it.
///
/// So the sweep now deletes only what it can prove it carried: the
/// head recorded at ASSEMBLY time must still be the branch's head.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SweepGuard {
    /// The branch still points at exactly what boarded.
    Delete,
    /// Commits arrived after boarding — the train never carried them,
    /// and they live nowhere else.
    Moved { recorded: String, current: String },
    /// The branch EXISTS and no head is on the record (a car that
    /// boarded before the conductor recorded one). An unknown head is
    /// not evidence: the cost of keeping a stale branch is a stale
    /// branch; the cost of deleting a moved one is lost work.
    NoRecord,
    /// The branch is not on the forge — nothing left to sweep.
    Gone,
}

/// The head-guard decision, pure. Both shas are full 40-char heads —
/// the assembly records what `git rev-parse` merged, the guard reads
/// what the forge names now — so equality is the whole test.
///
/// The forge's answer is read FIRST, and an absent branch settles the
/// question whatever the record says. Ordering the record first
/// conflates "we cannot vouch for this branch" with "there is no such
/// branch", and the second is not a finding: nothing to delete,
/// nothing to rescue, nothing an operator can do. Job 1bd1fb3d is the
/// bill — every car that boarded before this guard existed has
/// neither a recorded head nor a surviving branch, so the record-first
/// order made each one a `NoRecord` line on every reconcile, forever.
///
/// The reorder is free: `branch_head` was already called
/// unconditionally for every deletable branch, so the sweep asks the
/// forge exactly as often as it did before.
pub(crate) fn sweep_guard(recorded: Option<&str>, current: Option<&str>) -> SweepGuard {
    let recorded = recorded.filter(|s| !s.is_empty());
    let current = current.filter(|s| !s.is_empty());
    match (recorded, current) {
        (_, None) => SweepGuard::Gone,
        (None, Some(_)) => SweepGuard::NoRecord,
        (Some(r), Some(c)) if r == c => SweepGuard::Delete,
        (Some(r), Some(c)) => SweepGuard::Moved {
            recorded: r.to_string(),
            current: c.to_string(),
        },
    }
}

/// The journal line a guard verdict earns — `None` when it earns
/// none. Pure, so "what does the operator hear" is a decision with a
/// test rather than a shape buried in the sweep loop.
///
/// The sweep's journal is an operator surface, and a line belongs
/// there only when a human could act on it. `Gone` is not that: the
/// branch is not on the forge, so there is nothing to delete and
/// nothing to rescue. Job 1bd1fb3d is the cost of getting this wrong
/// — every car that boarded before the head guard existed has no
/// recorded head and no surviving branch, and narrating that pair put
/// dozens of lines in every reconcile, forever, about branches swept
/// by hand hours earlier.
///
/// `Delete` is silent here too, but for the opposite reason: the
/// caller does the deleting and is the only one who knows whether it
/// was a dry run, a deletion, or a race lost to something faster.
pub(crate) fn sweep_note(guard: &SweepGuard, b: &CarBranch) -> Option<String> {
    match guard {
        SweepGuard::Gone | SweepGuard::Delete => None,
        SweepGuard::NoRecord if b.rerail_origin => Some(format!(
            "rerail original {} has no head on record — not deleting (car {} landed)",
            b.branch,
            id8(&b.car)
        )),
        SweepGuard::NoRecord => Some(format!(
            "branch {} has no boarded head on record — not deleting (car {} landed)",
            b.branch,
            id8(&b.car)
        )),
        SweepGuard::Moved { recorded, current } => Some(branch_moved_line(b, recorded, current)),
    }
}

/// The line the sweep journals when a branch outgrew its boarding —
/// operator surface, and the only notice that unmerged commits are
/// sitting on a branch the train did not carry.
pub(crate) fn branch_moved_line(b: &CarBranch, recorded: &str, current: &str) -> String {
    let since = if b.rerail_origin {
        "rerail original"
    } else {
        "branch"
    };
    let what = if b.rerail_origin {
        "moved since the rerail"
    } else {
        "moved since boarding"
    };
    format!(
        "{since} {} {what} ({} -> {}) — not deleting",
        b.branch,
        id8(recorded),
        id8(current)
    )
}

/// The journal line one train earns when its sweep fails mid-flight —
/// a boarded car fetched and 404'd (the Job was deleted), a malformed
/// arrival-report write, a forge blip. Pure and named for the same
/// reason reconcile's per-train isolation names its train: the sweep
/// is best-effort per train, and best-effort without a named line is
/// just silent. An abort here USED to take the whole sweep down on a
/// `?`, so every later pending train went unswept and its landed
/// branch piled up on the forge (disk debt).
pub(crate) fn sweep_train_failed_line(train: &str, err: &anyhow::Error) -> String {
    format!(
        "sweep: train {} failed this pass — isolated, other trains continue: {err}",
        id8(train)
    )
}

/// The journal line one un-sweepable branch earns inside an otherwise
/// healthy train — a forge blip on `branch_head`/`delete_branch`, say.
/// Isolated per-branch so a single bad branch cannot strand the
/// train's OTHER landed branches on the forge; the train stays
/// unstamped so the branch is revisited next pass rather than leaked.
pub(crate) fn sweep_branch_failed_line(branch: &str, car: &str, err: &anyhow::Error) -> String {
    format!(
        "sweep: branch {branch} (car {}) failed — isolated, other branches continue: {err}",
        id8(car)
    )
}

/// A train's sweep is settled once every boarded car has reached a
/// terminal status — each branch is then deleted, deliberately kept
/// (main / a still-open car's claim), or the car never landed and
/// its branch outlives the train. A car still open keeps the train
/// on the sweep list for the next reconcile.
pub(crate) fn sweep_settled(boarded_cars: &[Value]) -> bool {
    boarded_cars.iter().all(|car| {
        matches!(
            car.get("status").and_then(Value::as_str),
            Some("closed") | Some("cancelled")
        )
    })
}

/// The closed trains the sweep still owes a visit: a train that
/// boarded something and carries no `branches_swept` stamp. Swept
/// trains and cancelled ones (nothing boarded, so no car branch to
/// delete) drop out here, fetch-free — the list rows carry metadata,
/// so this costs no per-car reads.
///
/// Pure, and separate from the read, because WHICH trains are pending
/// and HOW MANY rows the read gathered are different questions. The
/// leak this file was filed for came from answering the second one
/// with `limit=50`: cancelled trains close about once a minute when
/// the consist check is refusing, so a 50-row window turns over in
/// under an hour and a train whose car is proven later than that was
/// never looked at again.
pub(crate) fn sweep_pending(closed_trains: &[Value]) -> Vec<&Value> {
    closed_trains
        .iter()
        .filter(|t| {
            let md = t.get("metadata");
            !truthy(md.and_then(|m| m.get("branches_swept")))
                && truthy(md.and_then(|m| m.get("boarded_jobs")))
        })
        .collect()
}

/// Branches this pass withheld ONLY because a still-open car claims
/// the same name — the deferral `deletable_branches` makes when a
/// follow-up car rides a branch a landed car already used.
///
/// The deferral is right (the open car's work is unmerged) but it is
/// not final: the claim lifts the moment that car reaches a terminal,
/// and the branch becomes deletable then. So a deferred branch must
/// keep its train UNSTAMPED — stamping over it marks the train done
/// sweeping while one of its branches can still become deletable, and
/// the branch leaks for good. Same failure the `branch_failures`
/// guard exists to stop, reached by the other door.
pub(crate) fn claim_deferred_branches(
    boarded_cars: &[Value],
    open_branches: &BTreeSet<String>,
) -> Vec<CarBranch> {
    decided_branches(boarded_cars, |b| open_branches.contains(b))
}

/// The two decisions above differ in one predicate — whether a live
/// car's claim on the name is what we are looking for — so they share
/// the enumeration. Collapsed rather than copied: the copy is how the
/// deferral loop came to know about a car's boarded branch and not
/// about the branches it was re-railed off, which is the other half of
/// the leak this fix closes (CLAUDE.md §9a).
fn decided_branches(boarded_cars: &[Value], wanted: impl Fn(&str) -> bool) -> Vec<CarBranch> {
    let mut out: Vec<CarBranch> = Vec::new();
    for car in boarded_cars {
        for b in car_branches(car) {
            if !wanted(&b.branch) || out.iter().any(|o| o.branch == b.branch) {
                continue;
            }
            out.push(b);
        }
    }
    out
}

/// THE LEAK GUARD, in one predicate: a train may be stamped
/// `branches_swept` only when nothing it carries can still become
/// deletable. Three ways that is false, and each keeps the train on
/// the pending list for the next reconcile instead:
///   - a branch failed to sweep this pass (forge blip, 404 at
///     `get_job`) — revisit it;
///   - a branch was deferred to a still-open car's claim — the claim
///     lifts when that car closes;
///   - a boarded car has not reached a terminal — its branch becomes
///     deletable if it lands.
/// The stamp is what drops the train off the list, so stamping early
/// is not "a pass skipped", it is a branch left on the forge forever.
pub(crate) fn sweep_complete(
    branch_failures: usize,
    claim_deferred: usize,
    boarded_cars: &[Value],
) -> bool {
    branch_failures == 0 && claim_deferred == 0 && sweep_settled(boarded_cars)
}

/// A branch held back for a still-open car's claim says so: without a
/// line, an operator sees the same train re-swept every ten minutes
/// with nothing to show for it and no reason named.
pub(crate) fn claim_deferred_line(b: &CarBranch) -> String {
    format!(
        "sweep: {} kept — a still-open car claims it (car {} landed); train stays pending",
        sweep_subject(b),
        id8(&b.car)
    )
}

/// What the journal calls a branch the sweep acted on. A rerail
/// original is named as one: it was never a car of its own, which is
/// exactly why no earlier sweep could see it, and an operator reading
/// "deleted branch feat/x" for a branch no car ever carried has to go
/// re-derive where the deletion came from (§Diagnosis — a verdict must
/// name what it acted on).
pub(crate) fn sweep_subject(b: &CarBranch) -> String {
    if b.rerail_origin {
        format!("rerail original {}", b.branch)
    } else {
        format!("branch {}", b.branch)
    }
}

/// What a delete actually did, as OBSERVED — the forge's answer to
/// DELETE read against a `branch_head` taken afterwards. The answer
/// alone is a claim: 2xx says "deleted", 404 says "already gone", and
/// on 2026-09-11 six landed cars' branches were on the forge the next
/// morning under trains stamped `branches_swept`, with every condition
/// the sweep requires met — so the forge said one of those two things
/// and the branch stayed (backlog 1096b1a4). The merge is observed,
/// never assumed; so is the delete. `StillPresent` is a branch
/// failure: the train stays pending and the record names what the
/// forge said, so the next such morning is a one-read diagnosis
/// instead of a journal nobody outside the cluster can open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SweepDelete {
    /// DELETE answered success and the branch is gone.
    Deleted,
    /// DELETE answered "already gone" and the branch is gone.
    AlreadyGone,
    /// The branch is still on the forge after DELETE answered.
    StillPresent { forge_said: &'static str },
}

pub(crate) fn sweep_delete_verdict(claimed_deleted: bool, after: Option<&str>) -> SweepDelete {
    match (claimed_deleted, after.filter(|h| !h.is_empty())) {
        (true, None) => SweepDelete::Deleted,
        (false, None) => SweepDelete::AlreadyGone,
        (true, Some(_)) => SweepDelete::StillPresent {
            forge_said: "deleted",
        },
        (false, Some(_)) => SweepDelete::StillPresent {
            forge_said: "already gone",
        },
    }
}

/// One row of the train's `sweep_report`: the record of what the sweep
/// decided and observed for one branch, this pass. Stamped onto the
/// train beside `branches_swept` so the stamp carries its evidence.
pub(crate) fn sweep_report_row(b: &CarBranch, outcome: &str) -> Value {
    json!({
        "branch": b.branch,
        "car": id8(&b.car),
        "rerail_origin": b.rerail_origin,
        "outcome": outcome,
    })
}

/// The journal line for a delete the forge answered but did not
/// perform — the only kind of sweep line that must be loud.
pub(crate) fn sweep_still_present_line(b: &CarBranch, forge_said: &str, head: &str) -> String {
    format!(
        "sweep: {} STILL ON THE FORGE at {} after DELETE answered \"{forge_said}\" — not swept; train stays pending (car {} landed)",
        sweep_subject(b),
        &head[..head.len().min(8)],
        id8(&b.car)
    )
}

/// A step's `completed_at` evidence stamp, raw as stored. The
/// conductor stamps this on every step IT completes; steps closed by
/// other hands (the dispatcher's terminals) may not carry one.
fn step_stamp<'a>(train: &'a Value, slug: &str, title: &str) -> Option<&'a str> {
    find_step(train, slug, title)
        .and_then(|s| s.get("metadata"))
        .and_then(|m| m.get("completed_at"))
        .and_then(Value::as_str)
}

fn parse_stamp(s: Option<&str>) -> Option<DateTime<chrono::FixedOffset>> {
    s.and_then(|t| DateTime::parse_from_rfc3339(t).ok())
}

fn secs_between(
    a: Option<DateTime<chrono::FixedOffset>>,
    b: Option<DateTime<chrono::FixedOffset>>,
) -> Value {
    match (a, b) {
        (Some(a), Some(b)) => json!((b - a).num_seconds()),
        _ => Value::Null,
    }
}

/// The deployed sha out of the deploy step's summary evidence
/// (`main@<sha>; ...`). None when the summary is absent or shaped
/// differently — the report never guesses.
fn deployed_generation(summary: &str) -> Option<&str> {
    summary
        .strip_prefix("main@")
        .and_then(|rest| rest.split([';', ' ']).next())
        .filter(|sha| !sha.is_empty())
}

/// The arrival report — the landing's final structured entry, filed
/// on the `arrived` step when the sweep visits an arrived train.
/// Everything derives from evidence the job record already holds:
/// the boarded cars (consist), the board-time skips the train
/// recorded (left_behind), the deployed generation, and the timings
/// the conductor's own `completed_at` stamps make derivable. Missing
/// evidence reads as null, never a guess — `arrived_at` stays null
/// until whatever completes the outcome step stamps a time, and no
/// CI round count appears because the record does not carry one.
pub(crate) fn arrival_report(train: &Value, boarded_cars: &[Value]) -> Value {
    let consist: Vec<Value> = boarded_cars
        .iter()
        .map(|c| {
            json!({
                "car_id_short": id8(c.get("id").and_then(Value::as_str).unwrap_or("?")),
                "title": c.get("title").and_then(Value::as_str).unwrap_or_default(),
                "branch": c
                    .get("metadata")
                    .and_then(|m| m.get("branch"))
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
            })
        })
        .collect();
    let left_behind = train
        .get("metadata")
        .and_then(|m| m.get("left_behind"))
        .cloned()
        .unwrap_or_else(|| json!([]));
    let generation = find_step(train, "deployed", "Deployed to the playground")
        .and_then(|s| s.get("metadata"))
        .and_then(|m| m.get("deployed"))
        .and_then(Value::as_str)
        .and_then(deployed_generation);
    let merged_sha = find_step(train, "merged", "Merged into main")
        .and_then(|s| s.get("metadata"))
        .and_then(|m| m.get("merge_ref"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    let boarded = step_stamp(train, "collect", "Collect what is ready to board");
    let merged = step_stamp(train, "merged", "Merged into main");
    let deployed = step_stamp(train, "deployed", "Deployed to the playground");
    let arrived = step_stamp(train, "arrived", "Train arrived");
    let mut report = json!({
        "consist": consist,
        "left_behind": left_behind,
        "generation": generation,
        "timings": {
            "boarded_at": boarded,
            "merged_at": merged,
            "deployed_at": deployed,
            "arrived_at": arrived,
            "board_to_merge_s": secs_between(parse_stamp(boarded), parse_stamp(merged)),
            "merge_to_deploy_s": secs_between(parse_stamp(merged), parse_stamp(deployed)),
            "total_s": secs_between(parse_stamp(boarded), parse_stamp(arrived)),
        },
    });
    // The merged sha is the generation seen from the other end — a
    // short deploy sha prefixing the full merge sha is the SAME
    // commit, and repeating it would imply a divergence that is not
    // there. It appears only when genuinely distinct (or when the
    // deploy evidence is missing and it is the only sha on record).
    if let Some(m) =
        merged_sha.filter(|m| generation.is_none_or(|g| !(g.starts_with(m) || m.starts_with(g))))
    {
        report["merged_sha"] = json!(m);
    }
    report
}

/// The one-line form of the report — filed beside it as `summary`,
/// and the shape of the journal line. Reads the report, not the
/// world: unknowns print as "unknown" / "?", never as guesses.
pub(crate) fn arrival_summary(report: &Value) -> String {
    let n = report
        .get("consist")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    let generation = report
        .get("generation")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let total = report
        .get("timings")
        .and_then(|t| t.get("total_s"))
        .and_then(Value::as_i64)
        .map_or_else(|| "?".to_string(), |s| s.to_string());
    format!("{n} cars; generation {generation}; total {total}s")
}

/// Is a deploy actually needed? `current_key` is the generation
/// store's live key — the 8-char short-sha release dirname
/// (infra/generation.sh); `remote_main` is the FULL 40-char sha
/// `git ls-remote` answers. Same generation iff the full sha starts
/// with the short key — exactly that direction (the live incident:
/// this pair failing the comparison re-ran a full no-op deploy every
/// 10-minute reconcile). Missing evidence on either side deploys —
/// the deploy path surfaces its own errors, and a skip must never
/// rest on absence.
pub(crate) fn deploy_needed(current_key: &str, remote_main: &str) -> bool {
    current_key.is_empty() || remote_main.is_empty() || !remote_main.starts_with(current_key)
}

/// Is the conductor's own playground deploy turned OFF? An empty
/// `deploy_tree` (`BOSS_TRAIN_DEPLOY_TREE=""`) is the deliberate
/// config for the cluster-resident conductor: it has no `/opt/boss`
/// tree and no sudo, and the cluster converges on forge main by
/// itself — the forge-host cluster-deploy-runner takes the merge,
/// not the conductor (deployment-as-network; the migration in
/// docs/design/the-cluster-is-the-system.md). The default stays
/// `/opt/boss`, so the boss-gcp conductor is unaffected; only an
/// explicitly-empty tree disables the hop. Whitespace-only counts as
/// empty — it can only be a mis-set env var, never a real path.
pub(crate) fn playground_deploy_disabled(deploy_tree: &str) -> bool {
    deploy_tree.trim().is_empty()
}

/// The `deployed`-step evidence a cluster-resident conductor stamps
/// when it runs no playground deploy. It is a COMPLETION, not a
/// block: there is genuinely nothing for the conductor to deploy, and
/// the downstream convergence-verification step is what confirms the
/// cluster actually took the merge.
pub(crate) const NO_PLAYGROUND_DEPLOY_EVIDENCE: &str = "no playground deploy — the cluster converges on forge main via the deploy-runner \
     (deployment-as-network); nothing to deploy from the conductor";

/// Do two commit identifiers name the same commit? Shas arrive at
/// different lengths from different mouths — the merge_ref is the
/// forge's 12-char answer, `Capabilities.commit` is the full 40 the
/// image build baked in — so equality is prefix containment, gated at
/// >=7 chars a side so an empty or truncated report can never
/// accidentally "match".
pub(crate) fn commits_match(a: &str, b: &str) -> bool {
    a.len() >= 7 && b.len() >= 7 && (a.starts_with(b) || b.starts_with(a))
}

/// What a BLOCKED deploy tree should do this reconcile pass.
///
/// WHY THIS EXISTS. `deploy` refuses to build from a dirty or
/// off-main tree — correctly; deploying an unknown working state is
/// worse than waiting. But it only logged "deploy tree busy — will
/// retry" and stamped the step, so on 2026-09-02 the tree sat dirty
/// with a regenerated `Cargo.lock` and the conductor retried in
/// silence every ten minutes for SIX HOURS while two merged trains
/// waited to deploy. Nothing in the system of record said the
/// pipeline had stopped; it was found by reading a journal by hand.
///
/// This is the `ConvergenceVerdict::Overdue` idea one step upstream:
/// a quiet wait is fine, an INDEFINITE quiet wait is the defect.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum DeployBlockVerdict {
    /// Blocked, but inside the patience window — retry quietly.
    Waiting,
    /// Blocked past the window and nothing filed yet — file the packet.
    Overdue,
}

/// Pure so the rule is pinned by tests rather than by this comment.
/// `blocked_since` is the stamp the first blocked pass wrote; None
/// means this pass is the first, which is never overdue.
pub(crate) fn deploy_block_verdict(
    blocked_since: Option<DateTime<FixedOffset>>,
    now: DateTime<Utc>,
    alarm_after_mins: i64,
) -> DeployBlockVerdict {
    let Some(since) = blocked_since else {
        return DeployBlockVerdict::Waiting;
    };
    if (now.fixed_offset() - since).num_minutes() >= alarm_after_mins {
        DeployBlockVerdict::Overdue
    } else {
        DeployBlockVerdict::Waiting
    }
}

/// What the `converged` step should do this reconcile pass.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ConvergenceVerdict {
    /// The running cluster binary self-reports the merge commit —
    /// complete the step with that evidence.
    Converged,
    /// Not there yet and inside the patience window — say nothing,
    /// look again next pass.
    Waiting,
    /// Not there and past the window — file the loud packet (once).
    /// Waiting silently is the defect this verdict exists to end:
    /// measured at six unnoticed hours on 2026-08-19.
    Overdue,
}

/// The convergence decision, pure. `cluster_commit` is what the
/// cluster's health endpoint self-reported (None: unreachable, or a
/// binary from before the commit field existed — evidence of absence
/// is absence of evidence here, so it converges nothing and times out
/// like any other lag).
pub(crate) fn convergence_verdict(
    merge_ref: &str,
    cluster_commit: Option<&str>,
    merge_is_ancestor_of_cluster: Option<bool>,
    mins_since_merge: i64,
    alarm_after_mins: i64,
) -> ConvergenceVerdict {
    if let Some(c) = cluster_commit
        && commits_match(merge_ref, c)
    {
        return ConvergenceVerdict::Converged;
    }
    // Equality cannot see "the cluster rolled PAST this train". With
    // two trains in flight, the second's deploy overwrites the first's
    // evidence window: on 2026-09-02 train #176 wedged at converge
    // forever because the cluster self-reported #177's commit — which
    // CONTAINS #176's merge. Ancestry is the honest question ("does
    // the running commit include my merge"), answered by git at the
    // call site; None means git could not answer (no clone, unknown
    // commit) and converges nothing — absence of evidence, as ever.
    if merge_is_ancestor_of_cluster == Some(true) {
        return ConvergenceVerdict::Converged;
    }
    if mins_since_merge >= alarm_after_mins {
        ConvergenceVerdict::Overdue
    } else {
        ConvergenceVerdict::Waiting
    }
}

/// The live generation's key — the basename of the store's `current`
/// symlink. The store layout is owned by infra/generation.sh (the
/// one definition); this reads the same BOSS_GEN_ROOT contract.
/// Empty when the box has no generation store yet.
fn current_generation_key() -> String {
    let root = env_or("BOSS_GEN_ROOT", "/usr/local/boss");
    fs::read_link(Path::new(&root).join("current"))
        .ok()
        .and_then(|t| t.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_default()
}

/// The newest `completed_at` stamp across a train's steps — when
/// progress last provably happened. None when no step carries a
/// parseable stamp.
fn newest_completion(train: &Value) -> Option<&str> {
    train
        .get("steps")
        .and_then(Value::as_array)?
        .iter()
        .filter_map(|s| {
            let raw = s
                .get("metadata")
                .and_then(|m| m.get("completed_at"))
                .and_then(Value::as_str)?;
            Some((DateTime::parse_from_rfc3339(raw).ok()?, raw))
        })
        .max_by_key(|(t, _)| *t)
        .map(|(_, raw)| raw)
}

/// The stall sentinel's decision, pure: an open train counts stalled
/// when its newest step completion is at least `threshold_hours` old,
/// and the age in whole hours comes back for the journal line. No
/// completion evidence means no basis — None, never a guess.
pub(crate) fn stall_age_hours(
    train: &Value,
    now: DateTime<Utc>,
    threshold_hours: i64,
) -> Option<i64> {
    let newest = DateTime::parse_from_rfc3339(newest_completion(train)?).ok()?;
    let age = (now.signed_duration_since(newest)).num_hours();
    (age >= threshold_hours).then_some(age)
}

/// Which boarded cars a cancelled train releases back to the dock:
/// the still-open ones. A closed or cancelled car's record is history
/// — merged or abandoned, either way not the cancel path's to touch.
/// A CAR IS RELEASED ONLY IF IT STILL SAYS IT IS OURS.
///
/// Two facts answer "which cars are on this train": the train's
/// `boarded_jobs` list and each car's own `metadata.train`. Only the
/// second is maintained — releasing a car clears the car's marker and
/// leaves the train's list naming it forever. `parked_ready` and
/// `receipt_skip_reason` both read the CAR, so the car is authoritative
/// in practice while this function iterates the copy that drifts.
///
/// Cancelling a long-dead train therefore used to strip cars off a
/// LIVE one. Done on 2026-08-27: finishing the cancel of e1de28a3
/// released three cars that had since reboarded onto 1597b4a4, the next
/// board swept them onto a third train, and two trains believed they
/// carried the same consist. Nothing warned, because from inside the
/// loop a stale id and a current one look identical.
///
/// So the train's list proposes and the car disposes. A car naming a
/// different train has moved on; a car naming none was already released
/// and re-stamping it would overwrite a `skip_reason` that already says
/// where it has been.
pub(crate) fn releasable_cars<'a>(cars: &'a [Value], train_id: &str) -> Vec<&'a Value> {
    cars.iter()
        .filter(|c| c.get("status").and_then(Value::as_str) == Some("open"))
        .filter(|c| {
            c.get("metadata")
                .and_then(|m| m.get("train"))
                .and_then(Value::as_str)
                .is_some_and(|t| t == train_id)
        })
        .collect()
}

/// The auto-cancel decision, pure: should reconcile kill this train and
/// release its consist? Some(reason) or None, and the reason is what
/// lands on every released car.
///
/// A red train holds its whole consist hostage — the cars carry a
/// `train` marker so `parked_ready` no longer counts them, and the
/// conductor merges only on green, so nothing recovers on its own.
/// Overnight that is the difference between a pipeline that keeps
/// running and one that stops at the first fault. This is the reversal
/// of the older rule that only the operator may cancel (David,
/// 2026-08-15, choosing auto-cancel with a two-strike hold): raising is
/// still protocol, but an unattended pipeline has nobody to raise to.
///
/// THE VERDICT MUST BE THE LIVE ONE. `reconcile` reads it from the
/// forge each pass; the train's `ci` step keeps whatever verdict it was
/// first stamped with and is NOT re-stamped when CI re-runs. Deciding
/// from the step would cancel a train whose repair had already been
/// pushed and gone green — the exact case this is meant to rescue. A
/// re-running check reads `pending`, which is not `failing`, so a train
/// under repair is left alone.
///
/// A STALLED TRAIN IS RELEASED THE SAME WAY, AND FOR THE SAME REASON: a
/// run that was killed before it judged anything will never answer, and
/// its cars are no less hostage than a red train's. What differs is
/// whether the cancel counts against them — see `verdict_strikes_cars`.
pub(crate) fn auto_cancel_reason(
    train: &Value,
    live_verdict: &str,
    now: DateTime<Utc>,
    stall_hours: i64,
) -> Option<String> {
    let judged = match live_verdict {
        "failing" => true,
        "aborted" => false,
        _ => return None,
    };
    // A merged train is not a candidate whatever its checks say — the
    // content landed and the remaining steps are bookkeeping.
    if step_done(find_step(train, "merged", "Merged into main")) {
        return None;
    }
    let age = stall_age_hours(train, now, stall_hours)?;
    Some(if judged {
        format!(
            "CI red and no progress for {age}h (threshold {stall_hours}h) — cars released to board a later train"
        )
    } else {
        format!(
            "CI run aborted with no verdict and no progress for {age}h (threshold {stall_hours}h) — cars released unstruck to board a later train"
        )
    })
}

/// The operator's cancel stamp, parsed: `(reason, by)` when
/// `metadata.cancel_requested` is an object carrying a non-empty
/// `reason` and a non-empty `by`. Anything else — absent, a bare
/// string, an empty reason — is not a request and the conductor does
/// nothing on it: the reason lands on every released car as its
/// `skip_reason`, and a cancel that cannot say why or by whom is not
/// one the record can carry.
fn cancel_request(train: &Value) -> Option<(String, String)> {
    let req = train
        .get("metadata")?
        .get("cancel_requested")?
        .as_object()?;
    let reason = req.get("reason")?.as_str()?.trim();
    let by = req.get("by")?.as_str()?.trim();
    (!reason.is_empty() && !by.is_empty()).then(|| (reason.to_string(), by.to_string()))
}

/// The operator's cancel decision, pure — the yard's cancel button
/// (7a24caf3). An operator stamps the train's metadata
/// (`PATCH /api/jobs/{id}/metadata` with `cancel_requested: {by,
/// reason, at}`) and `reconcile` honours it on its next pass the way
/// it auto-cancels a stalled red — except that an operator's verb
/// NEVER strikes the cars (see `cancel`, the CLI verb). Some(reason)
/// is what lands on every released car.
///
/// A merged train is not a candidate whatever the stamp says: the
/// content landed, and releasing its cars would re-board changes that
/// are already on main. That train gets `operator_cancel_refusal`.
pub(crate) fn operator_cancel_reason(train: &Value) -> Option<String> {
    if step_done(find_step(train, "merged", "Merged into main")) {
        return None;
    }
    let (reason, by) = cancel_request(train)?;
    Some(format!("operator cancel: {reason} (by {by})"))
}

/// The answer a merged train owes a cancel request it cannot honour,
/// stamped ONCE as `cancel_refused` (like `ci_overdue_since`): the
/// request stays on the record, the refusal says why, and the yard has
/// something to render instead of a button that silently did nothing.
pub(crate) fn operator_cancel_refusal(train: &Value) -> Option<String> {
    let merged = find_step(train, "merged", "Merged into main");
    if truthy(train.get("metadata").and_then(|m| m.get("cancel_refused")))
        || !step_done(merged)
        || cancel_request(train).is_none()
    {
        return None;
    }
    let merge_ref = merged
        .and_then(|s| s.get("metadata"))
        .and_then(|m| m.get("merge_ref"))
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    Some(format!("already merged at {merge_ref}"))
}

/// Does this train's cancellation count against the cars aboard?
///
/// ONLY A RETURNED FAILING VERDICT. A strike is a claim that CI looked
/// at this consist and found it broken; two of them hold a car out of
/// the queue until a human looks (`car_hold_reason`). A run killed by an
/// infrastructure incident makes no such claim, and treating it as one
/// is how 2026-08-22 went: two trains stalled, their runs were cancelled
/// mid-flight, and the four cars aboard — every one of which test-merged
/// clean — took a strike on each train, hit the hold, and sat through
/// five departures before a human noticed.
///
/// The distinction lives on the release itself, not in a second counter:
/// the cars are released with the stall named in their `skip_reason`, so
/// the record says which question to ask without inventing a strike
/// nothing reads.
/// One forge commit status as one rollup entry — the ONLY place the
/// adapter decides which fields of a check the system of record keeps.
///
/// Pure, because this layer has dropped a field before and the drop was
/// silent: the check's NAME (`context`) was dropped, so every red train
/// in the SoR read `?:FAILURE` and 2026-09-02 cost two trips to the
/// forge API to learn that `test` had died on a disk floor, not on code.
/// The `description` is the other load-bearing field: locomotive.sh
/// posts a status whose description starts with `refused:` when the CI
/// host refuses BEFORE any check runs, and [`verdict_strikes_cars`]
/// spares every car aboard on that word alone (c186d63d). A rollup that
/// lost descriptions would turn every infrastructure refusal back into a
/// strike against innocent cars, silently. Pinned by
/// `a_refusal_survives_the_rollup_and_spares_the_cars`.
pub(crate) fn rollup_entry(st: &Value) -> Value {
    let verdict = st
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_lowercase();
    let conclusion = match verdict.as_str() {
        "success" => "SUCCESS",
        "failure" | "error" => "FAILURE",
        _ => "",
    };
    json!({
        "context": st.get("context").and_then(Value::as_str).unwrap_or_default(),
        // The forge's own one-line reason, when it gives one — free
        // provenance, and the refusal channel.
        "description": st
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or_default(),
        "conclusion": conclusion,
        "status": if verdict == "pending" { "PENDING" } else { "COMPLETED" },
        // …/actions/runs/{run}/jobs/{index}: the run and the failing
        // job's position in it. `attach_failing_logs` resolves it to a
        // job id and pulls the log tail.
        "target_url": st.get("target_url").and_then(Value::as_str).unwrap_or_default(),
    })
}

pub(crate) fn verdict_strikes_cars(verdict: &str, rollup: Option<&Value>) -> bool {
    if verdict != "failing" {
        return false;
    }
    // A failing verdict strikes UNLESS a failing check says it REFUSED.
    // The locomotive job posts a commit status whose description starts
    // with `refused:` when it declines to run — a disk floor, a stale
    // runner image, a wrong uid — before any check has judged the tree.
    // Three times (2026-08-22, 09-02, 09-05 train #204) that refusal
    // was recorded as a plain red and struck every car aboard; two
    // strikes hold a car out until a human looks. The word must sit on
    // a FAILING check: a passing check that mentions refusing proves
    // nothing, and a bare failure with no description is a real red.
    !any_failing_check_refused(rollup)
}

/// Does any FAILING check in the rollup carry a `refused` description?
pub(crate) fn any_failing_check_refused(rollup: Option<&Value>) -> bool {
    rollup
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|c| c.get("conclusion").and_then(Value::as_str) == Some("FAILURE"))
        .any(|c| {
            c.get("description")
                .and_then(Value::as_str)
                .is_some_and(|d| d.trim_start().to_lowercase().starts_with("refused"))
        })
}

/// The names of the checks that FAILED, from the rollup — `context`
/// (status checks) or `name` (check runs), whichever the entry carries.
pub(crate) fn failing_checks(rollup: Option<&Value>) -> Vec<String> {
    rollup
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|c| c.get("conclusion").and_then(Value::as_str) == Some("FAILURE"))
        .map(|c| {
            c.get("context")
                .or_else(|| c.get("name"))
                .and_then(Value::as_str)
                .unwrap_or("unnamed check")
                .to_string()
        })
        .collect()
}

// ---------------------------------------------------------------------------
// A red verdict names its LOG, not just its check (CLAUDE.md §Diagnosis,
// "a verdict must name what failed"). Naming `test` is half the answer;
// the operator still had to go re-derive WHY. These pure helpers turn a
// failing check into the id of the job that failed and a bounded tail of
// its log, resolving the id through the ONE endpoint that answers
// correctly. The forge seam does the fetching; everything decidable
// about a JSON payload is decided here, where it can be pinned.
//
// THE TRAP these encode around (live-confirmed 2026-09-06): the job log
// lives at `/actions/jobs/{JOB_ID}/logs`, but the JOB_ID must come from
// `/actions/runs/{run}/jobs` — NOT from `/actions/tasks`. A commit
// status's `target_url` is `…/actions/runs/{run}/jobs/{index}`, so it
// carries the run AND the job's position in that run: index into the
// run's jobs array and read its `id`. Reading the entry's `task_id`
// instead (the tasks-list id) silently returns a DIFFERENT job's log —
// run 462's failing `web` was job id 2008 / task_id 1939, and
// `jobs/1939/logs` was an unrelated SUCCESS. Position is unambiguous;
// name is only a cross-check.

/// How many bytes of a job log to pull (a suffix Range request — the
/// forge answers `206 Partial Content`, so a 300KB+ log never crosses
/// the wire whole) and how much of that to keep once fetched.
/// How much of a failing job's log to pull back from the forge.
///
/// This was 16 KB for as long as the alert only ever showed a TAIL. On
/// 2026-09-09 train 864a4896 reddened on a Rust test that panicked at
/// byte ~460 K of a 778 KB log; the last 16 KB held svelte-check
/// deprecation warnings from five minutes later, so the alert named the
/// check and then showed the reader something irrelevant to it, and the
/// real cause took a hand dig through the forge Actions API to find
/// (packet 792d26be). A suffix Range large enough to contain the whole
/// of any log this pipeline has produced is what lets
/// [`failing_excerpt`] find the failure at all; the bound still exists
/// so a pathological log cannot be pulled into memory unbounded.
pub(crate) const LOG_FETCH_BYTES: u64 = 8_388_608;
pub(crate) const LOG_TAIL_LINES: usize = 40;
pub(crate) const LOG_TAIL_BYTES: usize = 4_000;
/// Ceiling on the combined-logs blob stamped onto the `ci` step.
pub(crate) const CI_STEP_LOG_BYTES: usize = 8_000;

/// Parse `…/actions/runs/{run}/jobs/{index}` (a commit status's
/// `target_url`, relative or absolute) into `(run_id, job_index)`. The
/// job index is a position WITHIN that run's jobs array, not a job id.
/// `None` when the shape is absent — a url with a run but no `/jobs/{n}`
/// cannot be resolved to one job, and we never guess.
pub(crate) fn parse_run_job_ref(target_url: &str) -> Option<(i64, usize)> {
    let after = target_url.split("actions/runs/").nth(1)?;
    let mut parts = after.splitn(2, "/jobs/");
    let run: i64 = parts.next()?.trim_matches('/').parse().ok()?;
    let idx_part = parts.next()?;
    let idx_str: String = idx_part.chars().take_while(char::is_ascii_digit).collect();
    let index: usize = idx_str.parse().ok()?;
    Some((run, index))
}

/// Does a commit-status `context` name this job? `"CI / web
/// (pull_request)"` names job `"web"`: drop the `" (event)"` suffix,
/// take the segment after the last `" / "`, compare case-insensitively.
/// A cross-check on the positional resolution, never the resolution
/// itself — a mismatch downgrades the attachment to "resolved by
/// position, note the mismatch", it does not pick a different job.
pub(crate) fn context_names_job(context: &str, job_name: &str) -> bool {
    if job_name.trim().is_empty() {
        return false;
    }
    let ctx = context.to_lowercase();
    let ctx = ctx.split(" (").next().unwrap_or(&ctx);
    let seg = ctx.rsplit(" / ").next().unwrap_or(ctx).trim();
    seg == job_name.trim().to_lowercase()
}

/// The failing job's id, read POSITIONALLY from a run's jobs array (the
/// payload of `/actions/runs/{run}/jobs`). Returns the job's own `id`
/// (NEVER `task_id` — that is the trap), plus an optional note when the
/// job at that position does not obviously match the check context, so
/// an ambiguous mapping attaches what it can and SAYS it is unsure
/// rather than guessing a wrong job. `None` only when the index is out
/// of range or the entry carries no numeric `id`.
pub(crate) fn job_id_for_index(
    jobs: &[Value],
    index: usize,
    context: &str,
) -> Option<(i64, Option<String>)> {
    let job = jobs.get(index)?;
    let id = job.get("id").and_then(Value::as_i64)?;
    let name = job.get("name").and_then(Value::as_str).unwrap_or_default();
    let note = if context_names_job(context, name) {
        None
    } else {
        Some(format!(
            "resolved by target_url position (run job index {index}); \
             job name {name:?} did not obviously match check {context:?}"
        ))
    };
    Some((id, note))
}

/// Does a `Content-Range` header say the returned slice starts past
/// byte 0? A suffix Range fetch of a large log starts mid-line, so the
/// first line is a fragment to drop; a small log the server returned
/// whole (start 0, or no header) has a real first line to keep.
pub(crate) fn content_range_starts_past_zero(content_range: Option<&str>) -> bool {
    let Some(cr) = content_range else {
        return false;
    };
    // "bytes 303107-307106/307107" -> first number is the start.
    cr.trim()
        .strip_prefix("bytes")
        .unwrap_or(cr)
        .trim()
        .split('-')
        .next()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .is_some_and(|start| start > 0)
}

/// Reduce a raw log slice to a bounded, honest tail: drop a leading
/// partial line when the slice was a mid-file Range, keep the last
/// `max_lines`, then cap at `max_bytes` cutting on a line boundary. A
/// leading marker states it is a tail whenever anything was dropped, so
/// the words never claim to be the whole log (Orwell: the record says
/// what it is).
pub(crate) fn log_tail(
    body: &str,
    drop_partial_first: bool,
    max_lines: usize,
    max_bytes: usize,
) -> String {
    let body = body.trim_end_matches(['\n', '\r']);
    if body.is_empty() {
        return String::new();
    }
    let mut lines: Vec<&str> = body.lines().collect();
    let dropped_partial = drop_partial_first && lines.len() > 1;
    if dropped_partial {
        lines.remove(0);
    }
    let dropped_lines = lines.len() > max_lines;
    if dropped_lines {
        lines = lines.split_off(lines.len() - max_lines);
    }
    let mut out = lines.join("\n");
    let mut dropped_bytes = false;
    if out.len() > max_bytes {
        let start = out.len().saturating_sub(max_bytes);
        // Prefer the next newline after `start`; else the next char
        // boundary, so the slice is always valid UTF-8.
        let cut = out[start..]
            .find('\n')
            .map(|i| start + i + 1)
            .unwrap_or_else(|| {
                let mut s = start;
                while s < out.len() && !out.is_char_boundary(s) {
                    s += 1;
                }
                s
            });
        out = out[cut..].to_string();
        dropped_bytes = true;
    }
    if dropped_partial || dropped_lines || dropped_bytes {
        format!("…(log tail — earlier lines omitted)\n{out}")
    } else {
        out
    }
}

/// The lines a failing CI log is worth reading, in the order a reader
/// would grep for them. Each is unambiguous about a FAILURE: a Rust
/// panic and its location, cargo's per-target verdict, cargo's own
/// summary line, the gate runner's own refusal, a rustc error code, a
/// forge annotation, and bun's per-test failure marker.
///
/// Deliberately absent is a bare `error:`. The mocked web suite prints
/// hundreds of benign `error: Unable to connect` lines for backends it
/// does not run, and a marker that matches those points the excerpt at
/// noise — which is the defect this whole function exists to fix, in a
/// new place. Checked against both red logs of 2026-09-09: four matches
/// each across 9287 and 7300 lines, the first being the panic, and no
/// false positive.
pub(crate) const FAILURE_MARKERS: &[&str] = &[
    "panicked at",
    "test result: FAILED",
    "error: test failed",
    "GATE FAIL:",
    "error[E",
    "##[error]",
    "(fail)",
];

/// The excerpt a red-train alert should carry: the window around the
/// FIRST failure marker, or the tail when nothing matched.
///
/// A tail is what you show when you do not know what you are looking
/// for. Here the markers are known, so showing the end of the log
/// instead is a choice to hand the reader the wrong 40 lines — and on
/// 2026-09-09 it did exactly that, for a train carrying six cars that
/// had each gated green.
///
/// The first match is the one kept: a test run reports its earliest
/// failure first, and the later markers are usually that same failure
/// restated (the panic, then the target verdict, then cargo's summary,
/// then the gate's). How many matched is stated, so a reader who needs
/// the others knows they exist.
///
/// The result always says which of the two it is. A packet that shows a
/// tail while implying a diagnosis is the same defect one layer up.
pub(crate) fn failing_excerpt(
    body: &str,
    drop_partial_first: bool,
    max_lines: usize,
    max_bytes: usize,
) -> String {
    let trimmed = body.trim_end_matches(['\n', '\r']);
    if trimmed.is_empty() {
        return String::new();
    }
    let mut lines: Vec<&str> = trimmed.lines().collect();
    // A suffix Range starts mid-line; that fragment is not evidence.
    if drop_partial_first && lines.len() > 1 {
        lines.remove(0);
    }

    let hits: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, l)| FAILURE_MARKERS.iter().any(|m| l.contains(m)))
        .map(|(i, _)| i)
        .collect();

    let Some(&first) = hits.first() else {
        // Nothing named a failure. The tail is then the honest answer,
        // and it says so in its own words.
        return log_tail(body, drop_partial_first, max_lines, max_bytes);
    };

    // Lead-in enough to carry the test's name and the lines it printed
    // before dying. The window ENDS a few lines past the LAST marker
    // rather than running out its whole line budget: a Rust failure
    // restates itself four times in six lines and is then followed by
    // whatever else the job printed, and spending the budget on that is
    // how the reader ends up looking at svelte warnings again.
    let before = max_lines / 3;
    let last = *hits.last().unwrap_or(&first);
    let start = first.saturating_sub(before);
    let end = (last + 4)
        .min(lines.len())
        .min(start + max_lines)
        .max(first + 1);
    let mut out = lines[start..end].join("\n");

    if out.len() > max_bytes {
        let mut cut = max_bytes;
        while cut > 0 && !out.is_char_boundary(cut) {
            cut -= 1;
        }
        out.truncate(cut);
        out.push('…');
    }

    // Only failures the reader cannot see are worth mentioning. The
    // three or four lines a single Rust failure prints are all inside
    // the window, and announcing them as "3 more" would send someone
    // looking for a second failure that does not exist.
    let more = hits.iter().filter(|&&i| i >= end).count();
    let also = if more == 0 {
        String::new()
    } else if more == 1 {
        ", 1 more further down".to_string()
    } else {
        format!(", {more} more further down")
    };
    format!("…(the failure, with context{also})\n{out}")
}

/// The `(check, log_tail)` pairs the forge managed to attach to failing
/// rollup entries. Skips entries with no `log_tail` — which is exactly
/// the state a FAILED or ambiguous fetch leaves behind, so a fetch that
/// could not resolve a log yields no pair rather than an error. Used by
/// both the `ci` step's evidence and the red-train alert body.
pub(crate) fn failing_check_logs(rollup: Option<&Value>) -> Vec<(String, String)> {
    rollup
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|c| c.get("conclusion").and_then(Value::as_str) == Some("FAILURE"))
        .filter_map(|c| {
            let tail = c
                .get("log_tail")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())?;
            let ctx = c
                .get("context")
                .or_else(|| c.get("name"))
                .and_then(Value::as_str)
                .unwrap_or("unnamed check");
            Some((ctx.to_string(), tail.to_string()))
        })
        .collect()
}

/// Flatten the failing-check logs into one bounded blob for the `ci`
/// step's `check_logs` evidence field (`complete_step` stores strings).
/// Each block is headed by its check; the whole is capped so a step row
/// never carries megabytes.
pub(crate) fn format_check_logs(logs: &[(String, String)], max_bytes: usize) -> String {
    let mut out = String::new();
    for (ctx, tail) in logs {
        let block = format!("--- {ctx} ---\n{tail}\n");
        if !out.is_empty() && out.len() + block.len() > max_bytes {
            out.push_str("…(further check logs omitted)\n");
            break;
        }
        out.push_str(&block);
        if out.len() >= max_bytes {
            break;
        }
    }
    // A single block can still exceed the cap (a tail at its own limit
    // plus a long header). Hold the ceiling absolutely: keep the HEAD
    // here — the header names the check, and the per-check tail bound
    // already kept the log's end. Cut on a char boundary.
    if out.len() > max_bytes {
        let mut end = max_bytes;
        while end > 0 && !out.is_char_boundary(end) {
            end -= 1;
        }
        out.truncate(end);
        out.push('…');
    }
    out.trim_end().to_string()
}

/// A red train's self-announcement.
pub(crate) struct RedTrainAlert {
    pub(crate) title: String,
    pub(crate) failing: Vec<String>,
    pub(crate) refused: bool,
    /// `(check, log_tail)` for each failing check whose log the forge
    /// resolved — empty when none could be fetched, which is a missing
    /// attachment, never an error (observability is best-effort).
    pub(crate) logs: Vec<(String, String)>,
}

/// A red train announces itself IMMEDIATELY — pure over the LIVE verdict,
/// like [`auto_cancel_reason`], but it fires on the FIRST red pass rather
/// than waiting out the stall threshold, because the point is that a red
/// train is never a surprise (d69c4274, David: "red trains shouldn't
/// surprise us"). `Some` when the live CI verdict is `failing` and the
/// train has not merged; `None` otherwise. The alert NAMES the failing
/// checks and whether any was an infrastructure refusal, so the reader
/// sees WHAT failed without re-deriving it from the forge ("a verdict
/// must name what failed").
pub(crate) fn red_train_alert(
    train: &Value,
    live_verdict: &str,
    rollup: Option<&Value>,
) -> Option<RedTrainAlert> {
    if live_verdict != "failing" {
        return None;
    }
    // A merged train's checks are history; the content already landed.
    if step_done(find_step(train, "merged", "Merged into main")) {
        return None;
    }
    let failing = failing_checks(rollup);
    let refused = any_failing_check_refused(rollup);
    let id = train.get("id").and_then(Value::as_str).unwrap_or("");
    let named = if failing.is_empty() {
        "check names unavailable".to_string()
    } else {
        failing.join(", ")
    };
    let title = if refused {
        format!(
            "Red train {} — CI REFUSED (infrastructure, not the consist): {named}",
            id8(id)
        )
    } else {
        format!("Red train {} — CI failed: {named}", id8(id))
    };
    Some(RedTrainAlert {
        title,
        failing,
        refused,
        logs: failing_check_logs(rollup),
    })
}

/// The backlog-item body for a red-train alert. A FREE, PURE function so
/// it can be tested against the fields the jobs API demands — which is
/// exactly what the first cut of this alert got wrong: it omitted
/// `owner_id`, `status`, and `tags`, so every POST returned HTTP 422 and,
/// because reconcile filed the alert with `?`, the whole pass aborted at
/// rc=1. One red train then froze all landings for ~8h (2026-09-06). The
/// gate passed it because it only exercised `red_train_alert` (the pure
/// decision), never this body against the API. Now the body is pure and
/// pinned, and `reconcile` files it best-effort (see `announce_red_train`).
pub(crate) fn red_train_alert_body(tid: &str, alert: &RedTrainAlert) -> Value {
    json!({
        "kind": "backlog-item",
        "title": alert.title,
        "subject": {"subject_kind": "custom", "id": "bosspipeline"},
        "owner_id": "emp-david",
        "status": "open",
        "tags": [],
        "priority": "urgent",
        "metadata": {
            "title": alert.title,
            "train_alert": tid,
            "failing_checks": alert.failing,
            "refused": alert.refused,
            // The failing job's log tail, so the alert names not just
            // WHICH check failed but WHY — no hand-archaeology through
            // the forge. Empty when no log could be resolved.
            "failing_logs": alert.logs.iter()
                .map(|(check, tail)| json!({"check": check, "log_tail": tail}))
                .collect::<Vec<_>>(),
            "reporter": "conductor",
            "source": "pipeline-failure (red train)",
        }
    })
}

/// Has this train already filed its one red-train alert? The dedup is a
/// per-train metadata FLAG (`red_alert_filed`), mirroring
/// `deploy_alarm_filed` / `converge_alarm_filed` — NOT a scan of open
/// backlog-items. The scan it replaces read `status=open&limit=200` and
/// treated a truncated page as "no alert exists": once open
/// backlog-items passed 200 the existing alert sat beyond row 200, the
/// dedup answered "not raised", and the alert re-filed every ~10-min
/// reconcile pass — a self-compounding notification flood. A flag on the
/// train the caller already holds cannot truncate: it is one boolean.
pub(crate) fn red_alert_filed(train: &Value) -> bool {
    truthy(train.get("metadata").and_then(|m| m.get("red_alert_filed")))
}

#[cfg(test)]
mod red_train_alert_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_alert_body_carries_every_field_the_jobs_api_demands() {
        // The exact regression that froze the conductor for 8h: the body
        // must carry owner_id, status, and tags, or the POST is a 422.
        let alert = RedTrainAlert {
            title: "Red train abcd1234 — CI failed: CI / web".into(),
            failing: vec!["CI / web".into()],
            refused: false,
            logs: vec![],
        };
        let b = red_train_alert_body("abcd1234-0000-0000-0000-000000000000", &alert);
        for f in [
            "kind", "title", "subject", "owner_id", "status", "tags", "priority", "metadata",
        ] {
            assert!(
                b.get(f).is_some(),
                "the alert body must carry `{f}` — its absence returned HTTP 422 and froze reconcile"
            );
        }
        assert_eq!(b["status"], "open");
        assert_eq!(b["owner_id"], "emp-david");
        assert_eq!(b["tags"], json!([]));
        assert_eq!(
            b["metadata"]["train_alert"], "abcd1234-0000-0000-0000-000000000000",
            "the packet still names its train; dedup is the train's red_alert_filed flag"
        );
    }

    fn train(verdict_merged: bool) -> Value {
        let merged = if verdict_merged {
            "completed"
        } else {
            "pending"
        };
        json!({
            "id": "e799e241-aaaa-bbbb-cccc-000000000000",
            "steps": [{"metadata": {"spec_slug": "merged"}, "title": "Merged into main", "status": merged}]
        })
    }
    /// c186d63d: the sparing of innocent cars on a CI refusal rests on
    /// the rollup carrying each check's `description` — the same adapter
    /// layer that once dropped `context`. From the forge's statuses, as
    /// its API spells them, through the entry mapping, to the verdict.
    #[test]
    fn a_refusal_survives_the_rollup_and_spares_the_cars() {
        let forge_statuses = json!([
            {"context": "CI / build-image (pull_request)", "status": "success", "description": ""},
            {"context": "CI / locomotive refusal", "status": "failure",
             "description": "refused: 65GB free on the workspace filesystem, need 70GB",
             "target_url": "http://10.20.0.15:3000/david/boss/actions/runs/9/jobs/1"},
            {"context": "CI / test (pull_request)", "status": "pending", "description": ""},
        ]);
        let entries: Vec<Value> = forge_statuses
            .as_array()
            .unwrap()
            .iter()
            .map(super::rollup_entry)
            .collect();
        let refusal = &entries[1];
        assert_eq!(refusal["context"], "CI / locomotive refusal");
        assert_eq!(refusal["conclusion"], "FAILURE");
        assert_eq!(
            refusal["description"], "refused: 65GB free on the workspace filesystem, need 70GB",
            "the description is the refusal channel; dropping it turns the refusal into a strike"
        );
        assert_eq!(
            refusal["target_url"],
            "http://10.20.0.15:3000/david/boss/actions/runs/9/jobs/1"
        );
        assert_eq!(entries[2]["status"], "PENDING");
        let rollup = json!(entries);
        assert!(super::any_failing_check_refused(Some(&rollup)));
        assert!(
            !super::verdict_strikes_cars("failing", Some(&rollup)),
            "an infrastructure refusal must not strike the cars aboard"
        );
        // And the same statuses with the description lost DO strike — so
        // this test fails the moment the adapter drops the field again.
        let stripped: Vec<Value> = entries
            .iter()
            .map(|e| {
                let mut e = e.clone();
                e["description"] = json!("");
                e
            })
            .collect();
        assert!(super::verdict_strikes_cars(
            "failing",
            Some(&json!(stripped))
        ));
    }

    fn rollup(entries: Value) -> Value {
        entries
    }

    #[test]
    fn a_red_train_announces_the_failing_check() {
        let r = red_train_alert(
            &train(false),
            "failing",
            Some(&rollup(json!([
                {"context": "CI / build-image", "conclusion": "SUCCESS"},
                {"context": "CI / test", "conclusion": "FAILURE"}
            ]))),
        )
        .expect("a red train alerts");
        assert_eq!(r.failing, vec!["CI / test".to_string()]);
        assert!(!r.refused);
        assert!(
            r.title.contains("CI / test"),
            "title names the check: {}",
            r.title
        );
    }

    #[test]
    fn a_green_or_pending_verdict_is_no_alert() {
        assert!(red_train_alert(&train(false), "green", None).is_none());
        assert!(red_train_alert(&train(false), "pending", None).is_none());
    }

    #[test]
    fn a_merged_train_is_no_alert_whatever_the_verdict() {
        assert!(red_train_alert(&train(true), "failing", None).is_none());
    }

    #[test]
    fn the_red_alert_flag_suppresses_a_second_file() {
        // A train with no flag has not filed yet; the flag, once
        // stamped, makes `announce_red_train` a no-op. This is the whole
        // dedup — no scan of open backlog-items, so no page to truncate.
        let mut t = train(false);
        assert!(!red_alert_filed(&t), "an unflagged train has not alerted");
        t["metadata"] = json!({"red_alert_filed": true});
        assert!(
            red_alert_filed(&t),
            "the flag on the train must suppress a second file"
        );
    }

    #[test]
    fn an_infrastructure_refusal_is_named_as_such() {
        let r = red_train_alert(
            &train(false),
            "failing",
            Some(&rollup(json!([
                {"context": "CI / locomotive", "conclusion": "FAILURE", "description": "refused: disk floor"}
            ]))),
        )
        .expect("a refusal still alerts");
        assert!(r.refused);
        assert!(
            r.title.to_lowercase().contains("refused"),
            "title says refused: {}",
            r.title
        );
    }
}

#[cfg(test)]
mod red_verdict_log_tests {
    //! A red verdict names its LOG, not just its check. These pin the
    //! decidable parts: the run/job-id resolution off `target_url`, the
    //! log-tail truncation, and that a fetch that resolves nothing yields
    //! no attachment rather than an error. The effectful fetch itself
    //! (`fetch_job_log_tail` / `attach_failing_logs`) stays thin.
    use super::*;
    use serde_json::json;

    // ---- parse_run_job_ref -------------------------------------------

    #[test]
    fn a_target_url_yields_its_run_and_job_index() {
        // The exact shape this forge posts (relative), live-confirmed.
        assert_eq!(
            parse_run_job_ref("/david/boss/actions/runs/467/jobs/2"),
            Some((467, 2))
        );
        // Absolute form, and a trailing query/fragment after the index.
        assert_eq!(
            parse_run_job_ref("http://10.20.0.15:3000/david/boss/actions/runs/462/jobs/0?x=1"),
            Some((462, 0))
        );
    }

    #[test]
    fn a_url_without_a_job_index_resolves_to_nothing() {
        // A run alone cannot name ONE job — we never guess an index.
        assert_eq!(parse_run_job_ref("/david/boss/actions/runs/467"), None);
        assert_eq!(parse_run_job_ref("/david/boss/commit/abc"), None);
        assert_eq!(parse_run_job_ref(""), None);
    }

    // ---- job_id_for_index: the id, not the task_id -------------------

    fn run_jobs() -> Value {
        // A real /actions/runs/{run}/jobs payload: a plain array where
        // `id` is the JOB id (the logs key) and `task_id` is the TRAP.
        json!([
            {"id": 2031, "name": "build-image", "task_id": 1962, "status": "success"},
            {"id": 2032, "name": "locomotive",  "task_id": 1963, "status": "success"},
            {"id": 2033, "name": "web",         "task_id": 1964, "status": "failure"},
            {"id": 2034, "name": "fast",        "task_id": 1965, "status": "success"},
            {"id": 2035, "name": "test",        "task_id": 1966, "status": "failure"},
        ])
    }

    #[test]
    fn the_job_id_comes_from_position_and_is_the_id_not_the_task_id() {
        let jobs = run_jobs();
        let jobs = jobs.as_array().unwrap();
        // Index 2 == "web": id 2033 (the log key), NOT task_id 1964 —
        // passing 1964 to /actions/jobs/{id}/logs returns another job's
        // log (live-confirmed). Context matches the name, so no note.
        let (id, note) = job_id_for_index(jobs, 2, "CI / web (pull_request)").unwrap();
        assert_eq!(id, 2033, "the job id, never the task_id");
        assert!(note.is_none(), "name matched the context: {note:?}");

        let (id, note) = job_id_for_index(jobs, 4, "CI / test (pull_request)").unwrap();
        assert_eq!(id, 2035);
        assert!(note.is_none());
    }

    #[test]
    fn a_context_that_does_not_match_the_positioned_job_is_attached_with_a_note() {
        let jobs = run_jobs();
        let jobs = jobs.as_array().unwrap();
        // Positional resolution still yields a real job id, but the note
        // says the mapping was uncertain — attach what you can, don't
        // guess a different job.
        let (id, note) = job_id_for_index(jobs, 2, "CI / test (pull_request)").unwrap();
        assert_eq!(
            id, 2033,
            "position wins; we do not go hunting a 'better' job"
        );
        let note = note.expect("a mismatch is noted");
        assert!(note.contains("did not obviously match"), "note: {note}");
    }

    #[test]
    fn an_out_of_range_index_resolves_to_no_job() {
        let jobs = run_jobs();
        let jobs = jobs.as_array().unwrap();
        assert!(job_id_for_index(jobs, 9, "CI / whatever").is_none());
        assert!(job_id_for_index(&[], 0, "CI / web").is_none());
    }

    #[test]
    fn context_names_job_strips_workflow_and_event() {
        assert!(context_names_job("CI / web (pull_request)", "web"));
        assert!(context_names_job("CI / build-image (push)", "build-image"));
        assert!(context_names_job("CI / TEST (pull_request)", "test")); // case-insensitive
        assert!(!context_names_job("CI / web (pull_request)", "test"));
        assert!(!context_names_job("CI / web", ""));
    }

    // ---- content_range_starts_past_zero ------------------------------

    #[test]
    fn a_mid_file_range_is_detected_a_whole_body_is_not() {
        // A suffix Range of a big log starts past 0 -> first line is a
        // fragment to drop.
        assert!(content_range_starts_past_zero(Some(
            "bytes 303107-307106/307107"
        )));
        // The whole small file (start 0), or no Range at all (a 200):
        // the first line is real.
        assert!(!content_range_starts_past_zero(Some("bytes 0-4999/5000")));
        assert!(!content_range_starts_past_zero(None));
    }

    // ---- failing_excerpt: the failure, not the end -------------------

    /// The shape of a real red `test` job: thousands of lines of
    /// passing output, the panic in the middle, and five more minutes
    /// of unrelated warnings after it. Both red logs of 2026-09-09
    /// looked exactly like this.
    fn a_red_test_log() -> String {
        let mut l = String::new();
        for i in 0..3000 {
            l.push_str(&format!("2026-09-09T02:00:00Z passing line {i}\n"));
        }
        l.push_str("2026-09-09T02:04:46Z running 5 tests\n");
        l.push_str("2026-09-09T02:04:46Z test the_error_line ... FAILED\n");
        l.push_str("2026-09-09T02:04:46Z thread 'the_error_line' panicked at crates/core/boss-jobs/tests/station_boot_quarantine.rs:199:5:\n");
        l.push_str("2026-09-09T02:04:46Z an unviable active station is logged at ERROR: \n");
        l.push_str("2026-09-09T02:04:46Z test result: FAILED. 4 passed; 1 failed\n");
        l.push_str("2026-09-09T02:04:47Z GATE FAIL: test\n");
        for i in 0..2000 {
            l.push_str(&format!(
                "2026-09-09T02:06:13Z Warn: Using `on:submit` is deprecated {i}\n"
            ));
        }
        l
    }

    #[test]
    fn the_excerpt_carries_the_failure_not_the_end_of_the_log() {
        let out = failing_excerpt(&a_red_test_log(), false, 40, 4000);
        assert!(
            out.starts_with("…(the failure"),
            "says which of the two it is: {out}"
        );
        assert!(
            out.contains("panicked at crates/core/boss-jobs/tests/station_boot_quarantine.rs"),
            "the panic and its file are in the excerpt: {out}"
        );
        assert!(
            out.contains("test result: FAILED"),
            "cargo's verdict is in the excerpt: {out}"
        );
        // A few lines past the last marker are deliberate: a bun
        // failure prints its assertion diff there, and a gate its next
        // step. What must not happen is the excerpt BEING those lines,
        // which is what a 40-line tail of this log was.
        let warnings = out.matches("deprecated").count();
        assert!(
            warnings <= 3,
            "the warnings five minutes later are context at most, not the excerpt ({warnings} lines): {out}"
        );
        assert!(
            out.lines().filter(|l| l.contains("deprecated")).count() * 2 < out.lines().count(),
            "the failure, not the noise, is the bulk of what the reader sees: {out}"
        );
    }

    #[test]
    fn the_excerpt_leads_in_far_enough_to_name_the_test() {
        // The panic line names a file; the lines above it name the test
        // that produced it, and a reader needs both.
        let out = failing_excerpt(&a_red_test_log(), false, 40, 100_000);
        assert!(out.contains("running 5 tests"), "lead-in kept: {out}");
        assert!(out.contains("test the_error_line ... FAILED"));
    }

    #[test]
    fn how_many_failures_matched_is_stated() {
        let out = failing_excerpt(&a_red_test_log(), false, 40, 100_000);
        // All three markers of this one failure are inside the window,
        // so there is nothing further down to announce.
        assert!(
            !out.contains("further down"),
            "no phantom second failure is announced: {out}"
        );
        assert!(
            out.contains("GATE FAIL: test"),
            "the last marker is shown: {out}"
        );
    }

    #[test]
    fn a_second_failure_far_below_the_window_is_announced() {
        let mut body = String::new();
        body.push_str("panicked at the first place\n");
        for i in 0..200 {
            body.push_str(&format!("noise {i}\n"));
        }
        body.push_str("panicked at the second place\n");
        let out = failing_excerpt(&body, false, 40, 100_000);
        assert!(out.contains("the first place"), "the first is shown: {out}");
        assert!(
            out.contains("1 more further down"),
            "the second is announced, not hidden: {out}"
        );
        assert!(!out.contains("the second place"), "and not shown: {out}");
    }

    #[test]
    fn a_log_with_no_failure_marker_falls_back_to_the_tail_and_says_so() {
        let body: String = (0..500).map(|i| format!("row {i}\n")).collect();
        let out = failing_excerpt(&body, false, 40, 100_000);
        assert!(
            out.starts_with("…(log tail"),
            "an excerpt that is really a tail says it is a tail: {out}"
        );
        assert!(out.contains("row 499"), "the tail is the end: {out}");
    }

    #[test]
    fn the_benign_connection_errors_of_the_mocked_suite_are_not_a_failure() {
        // The mocked web suite prints hundreds of these for backends it
        // does not run, and every one of its tests passes. A marker set
        // that matched them would point the excerpt at noise.
        let mut body = String::new();
        for i in 0..200 {
            body.push_str("error: Unable to connect. Is the computer able to access the url?\n");
            body.push_str(&format!(
                "  ✓  {i} [chromium] › tests/mocked/thing.spec.ts\n"
            ));
        }
        let out = failing_excerpt(&body, false, 40, 100_000);
        assert!(
            out.starts_with("…(log tail"),
            "no failure was claimed where none is: {out}"
        );
    }

    #[test]
    fn the_excerpt_holds_its_byte_cap() {
        let mut body = String::new();
        body.push_str("panicked at somewhere\n");
        for i in 0..50 {
            body.push_str(&format!("{i}:{}\n", "x".repeat(500)));
        }
        let out = failing_excerpt(&body, false, 40, 1_000);
        assert!(out.len() <= 1_000 + 60, "byte-capped: {} bytes", out.len());
    }

    #[test]
    fn a_range_fetch_still_drops_the_partial_first_line() {
        let body = "ed at nothing\npanicked at real place\nafter\n";
        let out = failing_excerpt(body, true, 40, 4000);
        assert!(!out.contains("ed at nothing"), "fragment dropped: {out}");
        assert!(out.contains("panicked at real place"));
    }

    #[test]
    fn an_empty_log_stays_empty() {
        assert_eq!(failing_excerpt("", false, 40, 4000), "");
        assert_eq!(failing_excerpt("\n\n", false, 40, 4000), "");
    }

    // ---- log_tail: bounded and honest --------------------------------

    #[test]
    fn a_short_whole_log_is_returned_verbatim() {
        let body = "line a\nline b\nline c\n";
        let out = log_tail(body, false, 40, 4000);
        assert_eq!(out, "line a\nline b\nline c", "no marker on a whole log");
        assert!(!out.contains("omitted"));
    }

    #[test]
    fn a_range_fetch_drops_the_partial_first_line_and_marks_the_tail() {
        // The Range started mid-line, so "ne b" is a fragment.
        let body = "ne b\nline c\nline d";
        let out = log_tail(body, true, 40, 4000);
        assert!(out.starts_with("…(log tail"), "marked as a tail: {out}");
        assert!(!out.contains("ne b"), "partial first line dropped: {out}");
        assert!(out.contains("line c") && out.contains("line d"));
    }

    #[test]
    fn a_long_log_is_capped_to_the_last_lines() {
        let body: String = (0..500).map(|i| format!("row {i}\n")).collect();
        let out = log_tail(&body, false, 40, 100_000);
        let kept: Vec<&str> = out.lines().filter(|l| l.starts_with("row ")).collect();
        assert_eq!(kept.len(), 40, "kept the last 40 rows");
        assert_eq!(*kept.last().unwrap(), "row 499", "the tail, not the head");
        assert!(!kept.contains(&"row 459") || kept[0] == "row 460");
        assert!(out.contains("omitted"));
    }

    #[test]
    fn the_byte_cap_holds_even_when_the_line_count_is_fine() {
        let body: String = (0..10)
            .map(|i| format!("{i}:{}\n", "x".repeat(500)))
            .collect();
        let out = log_tail(&body, false, 40, 1_000);
        assert!(out.len() <= 1_000 + 40, "byte-capped: {} bytes", out.len());
        assert!(out.contains("omitted"));
        // The END is kept: the last line survives, the first does not.
        assert!(out.contains("9:"));
        assert!(!out.contains("0:xxxx"));
    }

    // ---- failing_check_logs / format_check_logs ----------------------

    #[test]
    fn failing_check_logs_reads_only_failing_entries_that_carry_a_tail() {
        let rollup = json!([
            {"context": "CI / build-image", "conclusion": "SUCCESS", "log_tail": "irrelevant"},
            {"context": "CI / web", "conclusion": "FAILURE", "log_tail": "boom on web"},
            {"context": "CI / test", "conclusion": "FAILURE"}, // fetch attached nothing
        ]);
        let logs = failing_check_logs(Some(&rollup));
        assert_eq!(
            logs,
            vec![("CI / web".to_string(), "boom on web".to_string())],
            "only the failing check WITH a tail; the success and the \
             no-tail failure contribute nothing"
        );
    }

    #[test]
    fn a_fetch_that_attached_nothing_is_a_missing_log_not_an_error() {
        // Exactly the rollup a FAILED or ambiguous log fetch leaves: a
        // FAILURE entry with no `log_tail`. The alert must still build,
        // carry the check name, and simply have no log — never error.
        let train = json!({
            "id": "e799e241-aaaa-bbbb-cccc-000000000000",
            "steps": [{"metadata": {"spec_slug": "merged"}, "title": "Merged into main", "status": "pending"}]
        });
        let rollup = json!([{"context": "CI / test", "conclusion": "FAILURE"}]);
        let alert = red_train_alert(&train, "failing", Some(&rollup))
            .expect("still an alert without a log");
        assert_eq!(alert.failing, vec!["CI / test".to_string()]);
        assert!(alert.logs.is_empty(), "no attachment, not an error");
        let body = red_train_alert_body("e799e241-aaaa-bbbb-cccc-000000000000", &alert);
        assert_eq!(
            body["metadata"]["failing_logs"],
            json!([]),
            "the body still forms, with an empty failing_logs"
        );
    }

    #[test]
    fn a_red_alert_carries_the_resolved_log_tail() {
        let train = json!({
            "id": "abcd1234-0000-0000-0000-000000000000",
            "steps": [{"metadata": {"spec_slug": "merged"}, "title": "Merged into main", "status": "pending"}]
        });
        let rollup = json!([
            {"context": "CI / test", "conclusion": "FAILURE", "log_tail": "assertion failed at line 9"}
        ]);
        let alert = red_train_alert(&train, "failing", Some(&rollup)).unwrap();
        assert_eq!(alert.logs.len(), 1);
        let body = red_train_alert_body("abcd1234-0000-0000-0000-000000000000", &alert);
        assert_eq!(body["metadata"]["failing_logs"][0]["check"], "CI / test");
        assert_eq!(
            body["metadata"]["failing_logs"][0]["log_tail"],
            "assertion failed at line 9"
        );
    }

    #[test]
    fn format_check_logs_heads_each_block_and_stays_bounded() {
        let logs = vec![
            ("CI / test".to_string(), "boom".to_string()),
            ("CI / web".to_string(), "kaboom".to_string()),
        ];
        let out = format_check_logs(&logs, 8_000);
        assert!(out.contains("--- CI / test ---"));
        assert!(out.contains("boom"));
        assert!(out.contains("--- CI / web ---"));

        // A tiny cap keeps the first block and stops, rather than
        // stamping megabytes onto the step.
        let big = vec![
            ("CI / test".to_string(), "x".repeat(5_000)),
            ("CI / web".to_string(), "y".repeat(5_000)),
        ];
        let out = format_check_logs(&big, 4_000);
        assert!(out.len() <= 4_000 + 80, "bounded: {} bytes", out.len());
        assert!(out.contains("--- CI / test ---"));
        assert!(!out.contains("--- CI / web ---"), "second block omitted");
    }
}

/// The metadata a released car carries away from a cancelled train.
///
/// `train`/`boarded_head` cleared so the dock counts it again, why it
/// came back, and — only when the train's CI actually judged it —
/// one more red against its record.
pub(crate) fn release_stamps(
    car: &Value,
    reason: &str,
    strike: bool,
) -> Vec<(&'static str, Value)> {
    // The boarded head goes with the train stamp: this car boarded
    // nothing now, and a stale head is not evidence about whatever it
    // boards next.
    let mut stamps = vec![
        ("train", Value::Null),
        ("boarded_head", Value::Null),
        (
            "skip_reason",
            json!(format!("returned to dock: train cancelled ({reason})")),
        ),
    ];
    // Every car aboard a red train is counted, not just the guilty one —
    // which car turned the consist red is exactly what nobody knows yet.
    // One red is survivable (see `car_hold_reason`); it takes a second,
    // aboard a DIFFERENT consist, before boarding holds it.
    if strike {
        let reds = car
            .get("metadata")
            .and_then(|m| m.get("red_trains"))
            .and_then(Value::as_i64)
            .unwrap_or(0)
            + 1;
        stamps.push(("red_trains", json!(reds)));
    }
    stamps
}

/// Has the CI verdict MOVED since it was last recorded?
///
/// THE BLIND SPOT THIS CLOSES. The `ci` step is completed exactly once,
/// the first time the rollup settles, and never looked at again. So the
/// verdict on a train that was repaired — pushed to, re-run, and gone
/// red a second time — is recorded nowhere and logged nowhere. On
/// 2026-08-15 train 20260815-0621 sat red for 45 minutes after a repair
/// with the system reporting nothing; it was found by querying the
/// forge by hand. The repair loop is exactly the path with no feedback,
/// which is the worst place to have none.
///
/// WHY THIS DOES NOT RE-STAMP THE STEP, which is the obvious fix and is
/// impossible: `update_step_at` freezes status, completed_on AND
/// METADATA on a terminal row, so the step's `result` cannot be
/// rewritten — and today's other lesson is not to design a path that
/// needs to un-complete a step. The train JOB's metadata is not frozen,
/// so the moving fact lives there, next to the immutable record of what
/// the verdict was when it first settled. Both are true and they are
/// different facts.
///
/// `pending` is never a change worth reporting: a re-run passes through
/// it on the way to an answer, and announcing it would make the signal
/// fire on every repair.
pub(crate) fn verdict_drift(recorded: Option<&str>, live: &str) -> Option<String> {
    let recorded = recorded?;
    if live == "pending" || live == recorded {
        return None;
    }
    Some(format!(
        "CI verdict moved {recorded} -> {live} since it was recorded"
    ))
}

/// CI has been asked and has not answered — the case `verdict_drift`
/// cannot see, because there is no verdict to compare.
///
/// Drift reports a verdict that MOVED. A runner that hangs, a job that
/// never reports, a queue nothing picks up: those produce no verdict at
/// all, so the train sits with its `ci` step incomplete and every
/// reconcile finds `pending` and says nothing. This is the backstop for
/// that, and only that.
///
/// THE THRESHOLD IS MEASURED, NOT GUESSED (David, 2026-08-15, choosing
/// 2x p90). Across 22 trains the pr->ci time had a median of ~33
/// minutes, a p90 of ~56, and a range of 10 to 169. Half again the
/// median would be ~50 minutes and would fire on six of those 22 — a
/// quarter of all trains, which is how an alert becomes furniture. Two
/// hours is roughly twice p90 and clears every train ever observed
/// except the 169-minute outlier, so when it fires it means something.
///
/// Worth recording alongside it, because it argues for LONGER trains:
/// that spread has no relationship to car count. A one-car train took
/// 63 minutes and an eight-car train took 12. The cost is per run, not
/// per car.
pub(crate) fn ci_overdue(
    train: &Value,
    now: DateTime<Utc>,
    threshold_hours: i64,
) -> Option<String> {
    // Only once the PR exists — before that there is nothing for CI to
    // answer about, and a train stuck earlier is the stall sentinel's.
    let asked = parse_stamp(step_stamp(train, "pr", "Open the batched PR"))?;
    if step_done(find_step(train, "ci", "CI verdict")) {
        return None;
    }
    let hours = now.signed_duration_since(asked).num_hours();
    (hours >= threshold_hours).then(|| {
        format!(
            "CI has not answered in {hours}h (threshold {threshold_hours}h) — no verdict, not a red one"
        )
    })
}

/// Why a train that is READY to merge is not being merged.
///
/// THE SILENT DECLINE THIS CLOSES. On 2026-09-04 a `boss train reconcile`
/// run by hand sat on a train with green CI and an OPEN PR and did
/// nothing — the merge arm requires `auto_merge`, which reads
/// `BOSS_TRAIN_AUTO_MERGE` from the environment, and a verb run by hand
/// inherits no unit (the conductor's ConfigMap is what sets it; see
/// §Doors). The else-branch was silence, so two reconciles reported a
/// clean pass while achieving nothing they were run for, and the
/// operator spent hours looking for a deeper fault that did not exist.
/// A conductor that declines to do the one thing it was run for owes an
/// answer, and the answer must name the reason: a verdict someone must
/// go re-derive is not a verdict.
///
/// ONLY THE GENUINELY-DECLINED CASE. Not-green and already-merged/closed
/// are the ordinary states of nearly every reconcile pass and are
/// reported elsewhere (`verdict_drift`, `ci_overdue`, the `merged` step);
/// naming them here would put a line on every train every ten minutes
/// and the signal would be furniture inside a day. Green + OPEN +
/// declined is the one state that looks like progress and is not.
pub(crate) fn merge_declined_reason(
    auto_merge: bool,
    verdict: &str,
    pr_state: Option<&str>,
) -> Option<&'static str> {
    if auto_merge || verdict != "green" || pr_state != Some("OPEN") {
        return None;
    }
    Some(
        "BOSS_TRAIN_AUTO_MERGE is not \"1\" (a verb run by hand inherits \
         no unit environment; the conductor's ConfigMap sets it)",
    )
}

/// The boarding hold, pure: a car released from that many red trains
/// stops boarding until someone looks at it. Without this the auto
/// cancel above is a loop — the same consist re-boards, goes red, and
/// cancels again all night, burning CI and landing nothing.
pub(crate) fn car_hold_reason(car: &Value, max_reds: i64) -> Option<String> {
    let reds = car
        .get("metadata")
        .and_then(|m| m.get("red_trains"))
        .and_then(Value::as_i64)
        .unwrap_or(0);
    (reds >= max_reds)
        .then(|| format!("held after {reds} red trains — needs a look before it boards again"))
}

/// The ONE branch a cancelled train may delete: its own `train/*`
/// assembly branch (the Job's subject id). Car branches hold the
/// cars' unmerged work and are never the cancel path's to touch —
/// this filter is the pin.
pub(crate) fn train_branch_to_delete(train: &Value) -> Option<String> {
    train
        .get("subject")
        .and_then(|s| s.get("id"))
        .and_then(Value::as_str)
        .filter(|b| b.starts_with("train/"))
        .map(str::to_string)
}

/// The branch an ARRIVED train sheds: its own `train/*` branch (the
/// same pin the cancel path deletes through), and only when the
/// record proves the happy landing — the `arrived` terminal strictly
/// `completed`, never `skipped`. A cancelled train closes with
/// `arrived` skipped, and its branch was the cancel verb's to delete
/// at cancel time.
///
/// `boss train cancel` has owned its branch since the verb existed;
/// nothing owned the branch after a HAPPY landing, and 62 stale
/// `train/*` branches accumulated on the forge between 08-13 and
/// 08-20 (packet ab3fa473). Squash merges are why nothing git-side
/// can ever classify them after the fact — the arrival record is the
/// proof, held here at exactly the right moment.
///
/// Gated on the forgejo adapter: the internal forge keeps merged PR
/// heads, which is the debt this cleans; GitHub auto-deletes them
/// repo-side, so under that adapter there is nothing to own.
pub(crate) fn arrival_branch_to_delete(train: &Value, forge_kind: &str) -> Option<String> {
    if forge_kind != "forgejo" {
        return None;
    }
    let arrived = find_step(train, "arrived", "Train arrived")?;
    (arrived.get("status").and_then(Value::as_str) == Some("completed"))
        .then(|| train_branch_to_delete(train))
        .flatten()
}

/// The journal line for an arrival cleanup's outcome — and the pin
/// that a failed delete is a LINE, never a failed arrival: the
/// `Result` is consumed here, so the caller has nothing left to
/// propagate. A leftover branch is housekeeping debt; a failed
/// arrival is an outage. Ok(false) (already gone) says nothing — the
/// sweep revisits an unsettled train every pass, and done work
/// narrated hourly reads as work happening.
pub(crate) fn arrival_cleanup_note(branch: &str, outcome: Result<bool>) -> Option<String> {
    match outcome {
        Ok(true) => Some(format!("deleted branch {branch} (train arrived)")),
        Ok(false) => None,
        Err(e) => Some(format!(
            "branch {branch} not deleted (arrival stands, debt noted): {e}"
        )),
    }
}

/// Resolve the operator's handle — a Job id, an id prefix, or the
/// train's PR url — against the open trains. Exactly one match or an
/// error saying what went wrong; an ambiguous prefix refuses rather
/// than guessing which train to cancel.
pub(crate) fn resolve_train<'a>(trains: &'a [Value], handle: &str) -> Result<&'a Value> {
    let matches: Vec<&Value> = trains
        .iter()
        .filter(|t| {
            let id = t.get("id").and_then(Value::as_str).unwrap_or_default();
            let pr_url = find_step(t, "pr", "Open the batched PR")
                .and_then(|s| s.get("metadata"))
                .and_then(|m| m.get("pr_url"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            id == handle || (!handle.is_empty() && id.starts_with(handle)) || pr_url == handle
        })
        .collect();
    match matches.as_slice() {
        [one] => Ok(one),
        [] => bail!("no open train matches {handle:?}"),
        many => bail!(
            "{handle:?} is ambiguous — matches trains {}",
            many.iter()
                .map(|t| id8(t.get("id").and_then(Value::as_str).unwrap_or("?")))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

// The DRY log lines mirror the python conductor's dict/list reprs —
// the journal is operator surface, and the port keeps its lines.

/// What a completed step announces: the step, the Job, and the
/// evidence just written to it.
///
/// The evidence half is the point. `complete_step` used to log only
/// `completed <step> on <id>`, which is byte-identical whether the
/// `ci` step recorded `result: green` or `result: failing`. Green was
/// loud only by accident — the merge path emits a second line — so a
/// red train produced strictly less output than a green one and read,
/// in the journal, as nothing having happened. Trains 46 and 47 both
/// went red inside an hour on 2026-08-16 and neither said so; the
/// second was missed by a log monitor that had already been widened
/// after the first (88c3890c).
///
/// Fields carrying nothing are omitted rather than printed as `None`
/// — most steps complete with no evidence at all, and a line ending
/// in `with {}` teaches readers to skip the tail of every line,
/// including the ones that matter.
fn completion_log_line(label: &str, id8: &str, fields: &[(&str, Option<String>)]) -> String {
    let evidence: Vec<(&str, Option<String>)> = fields
        .iter()
        .filter(|(_, v)| v.is_some())
        .cloned()
        .collect();
    if evidence.is_empty() {
        format!("completed {label} on {id8}")
    } else {
        format!("completed {label} on {id8} with {}", py_dict(&evidence))
    }
}

fn py_dict(fields: &[(&str, Option<String>)]) -> String {
    let inner = fields
        .iter()
        .map(|(k, v)| match v {
            Some(v) => format!("'{k}': '{v}'"),
            None => format!("'{k}': None"),
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!("{{{inner}}}")
}

fn py_keys(keys: &[&str]) -> String {
    let inner = keys
        .iter()
        .map(|k| format!("'{k}'"))
        .collect::<Vec<_>>()
        .join(", ");
    format!("[{inner}]")
}

fn py_pairs(cands: &[(Value, String)]) -> String {
    let inner = cands
        .iter()
        .map(|(j, b)| {
            let id = j.get("id").and_then(Value::as_str).unwrap_or("?");
            format!("('{}', '{b}')", id8(id))
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!("[{inner}]")
}

// ---------------------------------------------------------------------------
// The forge seam (internal-forge.md Q7a): every talk-to-the-code-host
// call goes through Forge, so internalizing Git/CI is an adapter swap
// — a ForgejoForge sibling selected by BOSS_TRAIN_FORGE — instead of
// a conductor rewrite at cutover. The GitHub adapter shells to `gh`
// exactly as before; behavior is unchanged by this refactor.
// ---------------------------------------------------------------------------

/// The code host as the conductor sees it: five verbs.
#[async_trait]
trait Forge: Send + Sync {
    /// -> {state, mergeCommit, statusCheckRollup} for a PR url.
    async fn pr_info(&self, url: &str) -> Result<Value>;
    /// Open a PR head->main on repo; return its url.
    async fn pr_create(
        &self,
        repo: &str,
        head_branch: &str,
        title: &str,
        body: &str,
    ) -> Result<String>;
    async fn merge(&self, url: &str) -> Result<()>;
    /// Close a PR WITHOUT merging — a cancelled train's PR must not
    /// sit open inviting a merge.
    async fn close_pr(&self, url: &str) -> Result<()>;
    /// Delete `branch` from the repo car branches are pushed to.
    /// Ok(true) = deleted; Ok(false) = already gone (404) — an
    /// expected state, the repo auto-deletes merged `train/*` PR
    /// heads and hand sweeps happen. Anything else is an error.
    async fn delete_branch(&self, branch: &str) -> Result<bool>;
    /// The branch's head sha right now, or Ok(None) when the branch is
    /// not there (404). The sweep's head guard reads this: a landed
    /// car's branch is only deletable while it still points at what
    /// boarded.
    async fn branch_head(&self, branch: &str) -> Result<Option<String>>;
    /// Cancel the still-running CI runs belonging to this train, and
    /// say how many were cancelled.
    ///
    /// Cancelling a train releases its cars and closes its PR but used
    /// to leave the run burning: measured 2026-08-17, a job for the
    /// cancelled train 58 was still running 27 minutes later, holding
    /// 78.65GB across three volumes (`docker system df` reporting 0B
    /// reclaimable is the tell — they are attached to a LIVE
    /// container), and the runner is single-concurrency, so the next
    /// train's jobs all sat in `waiting` behind work for a train that
    /// no longer existed. The forge host fell from 136G free to 44G,
    /// under locomotive's own 70GB floor, so the following train would
    /// have red-ed on a preflight telling the truth about a condition
    /// nobody caused. Packet `89b27e60`.
    ///
    /// There is no rerun API on this forge, but cancel works — probed
    /// against an already-finished run so the probe could not disturb
    /// live work.
    async fn cancel_ci_runs(&self, pr_index: &str, head_sha: &str) -> Result<usize>;
}

/// `owner/name` from a clone url — https or ssh, with or without
/// `.git`: `https://github.com/dauld/boss-fork.git` and
/// `git@github.com:dauld/boss-fork` both give `dauld/boss-fork`.
pub(crate) fn repo_path(url: &str) -> String {
    let u = url.trim_end_matches('/').trim_end_matches(".git");
    let mut segs = u.rsplit(['/', ':']);
    let name = segs.next().unwrap_or_default();
    let owner = segs.next().unwrap_or_default();
    format!("{owner}/{name}")
}

struct GitHubForge {
    head_owner: String,
    /// The fork holding car branches (`owner/name`) — under GitHub
    /// the cars push to the fork, so that is where a landed car's
    /// branch gets deleted from.
    fork_repo: String,
}

#[async_trait]
impl Forge for GitHubForge {
    async fn pr_info(&self, url: &str) -> Result<Value> {
        let r = sh(&[
            "gh",
            "pr",
            "view",
            url,
            "--json",
            "state,mergeCommit,statusCheckRollup",
        ])?;
        serde_json::from_str(&stdout_str(&r)).context("parsing gh pr view output")
    }

    async fn pr_create(
        &self,
        repo: &str,
        head_branch: &str,
        title: &str,
        body: &str,
    ) -> Result<String> {
        let head = format!("{}:{head_branch}", self.head_owner);
        let r = sh(&[
            "gh", "pr", "create", "--repo", repo, "--head", &head, "--base", "main", "--title",
            title, "--body", body,
        ])?;
        let out = stdout_str(&r);
        Ok(out.trim().lines().last().unwrap_or_default().to_string())
    }

    async fn merge(&self, url: &str) -> Result<()> {
        sh(&["gh", "pr", "merge", url, "--squash"])?;
        Ok(())
    }

    async fn close_pr(&self, url: &str) -> Result<()> {
        sh(&["gh", "pr", "close", url])?;
        Ok(())
    }

    async fn delete_branch(&self, branch: &str) -> Result<bool> {
        let path = format!("repos/{}/git/refs/heads/{branch}", self.fork_repo);
        let r = sh_unchecked(&["gh", "api", "--method", "DELETE", &path])?;
        if r.status.success() {
            return Ok(true);
        }
        let stderr = String::from_utf8_lossy(&r.stderr);
        if stderr.contains("HTTP 404") || stderr.contains("Not Found") {
            return Ok(false);
        }
        bail!("gh api DELETE {path}: {}", stderr.trim());
    }

    /// `git/ref/heads/<branch>` — the singular form, which answers
    /// with the ONE ref; the plural `git/refs/...` answers with every
    /// ref sharing the prefix, and `feat/x` would happily return
    /// `feat/x-followup`.
    async fn branch_head(&self, branch: &str) -> Result<Option<String>> {
        let path = format!("repos/{}/git/ref/heads/{branch}", self.fork_repo);
        let r = sh_unchecked(&["gh", "api", &path])?;
        if !r.status.success() {
            let stderr = String::from_utf8_lossy(&r.stderr);
            if stderr.contains("HTTP 404") || stderr.contains("Not Found") {
                return Ok(None);
            }
            bail!("gh api {path}: {}", stderr.trim());
        }
        let v: Value =
            serde_json::from_str(&stdout_str(&r)).context("parsing gh api git/ref output")?;
        Ok(v.get("object")
            .and_then(|o| o.get("sha"))
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string))
    }

    /// GitHub keys runs by head sha, which `gh run list --commit`
    /// takes directly — so this adapter does not need
    /// `cancellable_run_ids`, whose whole job is working around the
    /// Forgejo shape. A failure here is logged, never propagated: the
    /// pipeline does not run on this adapter any more, and a cancel
    /// that cannot reach GitHub must still release the cars.
    async fn cancel_ci_runs(&self, _pr_index: &str, head_sha: &str) -> Result<usize> {
        if head_sha.is_empty() {
            return Ok(0);
        }
        let r = sh_unchecked(&[
            "gh",
            "run",
            "list",
            "--commit",
            head_sha,
            "--json",
            "databaseId,status",
        ])?;
        if !r.status.success() {
            log(format!(
                "cancel: could not list GitHub runs for {head_sha}: {}",
                String::from_utf8_lossy(&r.stderr).trim()
            ));
            return Ok(0);
        }
        let runs: Vec<Value> = serde_json::from_str(&stdout_str(&r)).unwrap_or_default();
        let mut cancelled = 0;
        for run in runs {
            let status = run
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if !["in_progress", "queued", "waiting", "requested", "pending"].contains(&status) {
                continue;
            }
            let Some(id) = run.get("databaseId").and_then(Value::as_i64) else {
                continue;
            };
            let out = sh_unchecked(&["gh", "run", "cancel", &id.to_string()])?;
            if out.status.success() {
                cancelled += 1;
            }
        }
        Ok(cancelled)
    }
}

/// The same five verbs against the internal forge's API. PRs are
/// same-repo (no fork dance): the train branch pushes to the one
/// repo, the PR head is the bare branch name, and car branches get
/// deleted from that same repo at arrival.
struct ForgejoForge {
    base: String,
    repo: String,
    token: String,
    http: reqwest::Client,
}

impl ForgejoForge {
    fn new() -> Result<Self> {
        let base = env_or("BOSS_TRAIN_FORGE_URL", "http://10.20.0.15:3000")
            .trim_end_matches('/')
            .to_string();
        let repo = env_or("BOSS_TRAIN_FORGE_REPO", "david/boss");
        let token_file = env_or("BOSS_TRAIN_FORGE_TOKEN_FILE", "/etc/boss-train/forge.token");
        let token = fs::read_to_string(&token_file)
            .with_context(|| format!("reading {token_file}"))?
            .trim()
            .to_string();
        Ok(ForgejoForge {
            base,
            repo,
            token,
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .build()?,
        })
    }

    async fn api(
        &self,
        method: Method,
        path: &str,
        payload: Option<Value>,
    ) -> Result<Option<Value>> {
        let mut req = self
            .http
            .request(method.clone(), format!("{}/api/v1{path}", self.base))
            .header("Authorization", format!("token {}", self.token))
            .header("Content-Type", "application/json");
        if let Some(p) = &payload {
            req = req.json(p);
        }
        let resp = req
            .send()
            .await
            .with_context(|| format!("forge {method} {path}"))?;
        let status = resp.status();
        let body = resp.text().await?;
        if !status.is_success() {
            bail!("forge {method} {path}: HTTP {status}: {}", body.trim());
        }
        if body.trim().is_empty() {
            Ok(None)
        } else {
            Ok(Some(serde_json::from_str(&body).with_context(|| {
                format!("parsing forge {method} {path} response")
            })?))
        }
    }

    fn index(url: &str) -> String {
        url.trim_end_matches('/')
            .rsplit('/')
            .next()
            .unwrap_or_default()
            .to_string()
    }

    /// For each FAILING rollup entry, resolve the failing job's id from
    /// its `target_url` and attach a bounded tail of that job's log, so a
    /// red verdict carries WHY, not just WHICH check. This is the
    /// effectful seam; every decision about a JSON payload lives in the
    /// pure helpers above (`parse_run_job_ref`, `job_id_for_index`,
    /// `log_tail`), which are what the tests pin.
    ///
    /// STRICTLY BEST-EFFORT — this is observability. Every failure logs
    /// and continues, leaving the entry with no `log_tail`; nothing here
    /// can abort `pr_info`, the verdict, or reconcile. A run's jobs are
    /// fetched once and shared across the checks that name it (a red
    /// train usually has one run).
    async fn attach_failing_logs(&self, rollup: &mut [Value]) {
        // Phase 1: fetch each DISTINCT run's jobs once. Kept separate
        // from the attach phase so the cache insert is a plain statement,
        // not a `contains_key`-then-insert in the loop (and so no
        // `HashMap::Entry` is held across the `.await`).
        let runs: BTreeSet<i64> = rollup
            .iter()
            .filter(|e| e.get("conclusion").and_then(Value::as_str) == Some("FAILURE"))
            .filter_map(|e| e.get("target_url").and_then(Value::as_str))
            .filter_map(|u| parse_run_job_ref(u).map(|(run, _)| run))
            .collect();
        let mut run_jobs: HashMap<i64, Vec<Value>> = HashMap::new();
        for run in runs {
            match self
                .api(
                    Method::GET,
                    &format!("/repos/{}/actions/runs/{run}/jobs", self.repo),
                    None,
                )
                .await
            {
                Ok(v) => {
                    let jobs = v
                        .as_ref()
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default();
                    run_jobs.insert(run, jobs);
                }
                Err(e) => log(format!(
                    "red-log: run {run} jobs list failed (non-fatal): {e}"
                )),
            }
        }
        // Phase 2: for each failing check, resolve its job id positionally
        // and attach a bounded tail of that job's log.
        for entry in rollup.iter_mut() {
            if entry.get("conclusion").and_then(Value::as_str) != Some("FAILURE") {
                continue;
            }
            let ctx = entry
                .get("context")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let target = entry
                .get("target_url")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let Some((run, index)) = parse_run_job_ref(target) else {
                log(format!(
                    "red-log: no run/job in target_url {target:?} for {ctx:?} — no log attached"
                ));
                continue;
            };
            let jobs = run_jobs.get(&run).map(Vec::as_slice).unwrap_or(&[]);
            let Some((job_id, note)) = job_id_for_index(jobs, index, &ctx) else {
                log(format!(
                    "red-log: job index {index} out of range for run {run} ({ctx:?}) \
                     — no log attached"
                ));
                continue;
            };
            match self.fetch_job_log_tail(job_id).await {
                Ok(tail) if !tail.is_empty() => {
                    entry["log_tail"] = json!(tail);
                    entry["log_job_id"] = json!(job_id);
                    if let Some(n) = note {
                        entry["log_note"] = json!(n);
                    }
                }
                Ok(_) => {}
                Err(e) => log(format!(
                    "red-log: job {job_id} log fetch failed for {ctx:?} (non-fatal): {e}"
                )),
            }
        }
    }

    /// Pull the tail of a job's log. A suffix `Range` request keeps a
    /// 300KB+ (test jobs: far more) log off the wire — the forge answers
    /// `206 Partial Content` — and the raw text (the logs endpoint serves
    /// `text/plain`, not JSON, so it bypasses `api`) is reduced to a
    /// bounded tail. The job id MUST be the run-jobs `id`, never the
    /// tasks-list id; see `attach_failing_logs`.
    async fn fetch_job_log_tail(&self, job_id: i64) -> Result<String> {
        let url = format!(
            "{}/api/v1/repos/{}/actions/jobs/{job_id}/logs",
            self.base, self.repo
        );
        let resp = self
            .http
            .get(&url)
            .header("Authorization", format!("token {}", self.token))
            .header("Range", format!("bytes=-{LOG_FETCH_BYTES}"))
            .send()
            .await
            .with_context(|| format!("forge GET job {job_id} logs"))?;
        let status = resp.status();
        // 200 (whole, small log) and 206 (Range honoured) both succeed.
        if !status.is_success() {
            bail!("forge GET job {job_id} logs: HTTP {status}");
        }
        let partial = content_range_starts_past_zero(
            resp.headers()
                .get(reqwest::header::CONTENT_RANGE)
                .and_then(|v| v.to_str().ok()),
        );
        // Forgejo pads job logs with NUL bytes; they survive into the
        // packet as \u0000 and make an excerpt unreadable.
        let body = resp.text().await?.replace('\0', "");
        Ok(failing_excerpt(
            &body,
            partial,
            LOG_TAIL_LINES,
            LOG_TAIL_BYTES,
        ))
    }
}

#[async_trait]
impl Forge for ForgejoForge {
    /// Shape Forgejo's PR + combined status into the exact dict the
    /// GitHub adapter returns, so reconcile stays forge-blind.
    async fn pr_info(&self, url: &str) -> Result<Value> {
        let idx = Self::index(url);
        let pr = self
            .api(
                Method::GET,
                &format!("/repos/{}/pulls/{idx}", self.repo),
                None,
            )
            .await?
            .ok_or_else(|| anyhow!("empty PR body for {url}"))?;
        let state = if truthy(pr.get("merged")) {
            "MERGED"
        } else if pr.get("state").and_then(Value::as_str) == Some("open") {
            "OPEN"
        } else {
            "CLOSED"
        };
        let head_sha = pr
            .get("head")
            .and_then(|h| h.get("sha"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let mut rollup = Vec::new();
        if !head_sha.is_empty() {
            let combined = self
                .api(
                    Method::GET,
                    &format!("/repos/{}/commits/{head_sha}/status", self.repo),
                    None,
                )
                .await?;
            let statuses = combined
                .as_ref()
                .and_then(|c| c.get("statuses"))
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            for st in &statuses {
                rollup.push(rollup_entry(st));
            }
        }
        // A red verdict names its log, not just its check: best-effort,
        // and only for the FAILING entries, so a green train pays nothing.
        self.attach_failing_logs(&mut rollup).await;
        Ok(json!({
            "state": state,
            "mergeCommit": {
                "oid": pr.get("merge_commit_sha").and_then(Value::as_str).unwrap_or_default()
            },
            "statusCheckRollup": rollup,
        }))
    }

    async fn pr_create(
        &self,
        _repo: &str,
        head_branch: &str,
        title: &str,
        body: &str,
    ) -> Result<String> {
        let pr = self
            .api(
                Method::POST,
                &format!("/repos/{}/pulls", self.repo),
                Some(json!({
                    "head": head_branch, "base": "main",
                    "title": title, "body": body,
                })),
            )
            .await?
            .ok_or_else(|| anyhow!("empty create-PR response from the forge"))?;
        pr.get("html_url")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| anyhow!("create-PR response without html_url"))
    }

    async fn merge(&self, url: &str) -> Result<()> {
        let idx = Self::index(url);
        self.api(
            Method::POST,
            &format!("/repos/{}/pulls/{idx}/merge", self.repo),
            Some(json!({"Do": "squash"})),
        )
        .await?;
        Ok(())
    }

    async fn close_pr(&self, url: &str) -> Result<()> {
        let idx = Self::index(url);
        self.api(
            Method::PATCH,
            &format!("/repos/{}/pulls/{idx}", self.repo),
            Some(json!({"state": "closed"})),
        )
        .await?;
        Ok(())
    }

    /// DELETE /repos/{owner}/{repo}/branches/{branch}. Not through
    /// `api()` — a 404 here is an answer (already gone), not an
    /// error, and `api()` bails on every non-2xx.
    async fn delete_branch(&self, branch: &str) -> Result<bool> {
        let resp = self
            .http
            .request(
                Method::DELETE,
                format!("{}/api/v1/repos/{}/branches/{branch}", self.base, self.repo),
            )
            .header("Authorization", format!("token {}", self.token))
            .send()
            .await
            .with_context(|| format!("forge DELETE branches/{branch}"))?;
        let status = resp.status();
        if status == reqwest::StatusCode::NOT_FOUND {
            return Ok(false);
        }
        let body = resp.text().await?;
        // Forgejo answers a DELETE of an absent branch with 500 and
        // `object does not exist [id: refs/heads/<b>]`, not 404 —
        // observed 2026-08-13 against branches removed out of band,
        // where it failed every reconcile AFTER the merge and deploy
        // had already succeeded, so the run reported rc=1 and re-filed
        // its arrival report each tick. Already-gone is the sweep's
        // success condition whatever status dresses it up.
        if !status.is_success() && body.contains("object does not exist") {
            return Ok(false);
        }
        if !status.is_success() {
            bail!(
                "forge DELETE /repos/{}/branches/{branch}: HTTP {status}: {}",
                self.repo,
                body.trim()
            );
        }
        Ok(true)
    }

    /// GET /repos/{owner}/{repo}/branches/{branch} — `commit.id` is
    /// the head. Not through `api()` for the same reason as the delete
    /// above: a 404 here is an answer (no such branch), not an error.
    async fn branch_head(&self, branch: &str) -> Result<Option<String>> {
        let resp = self
            .http
            .request(
                Method::GET,
                format!("{}/api/v1/repos/{}/branches/{branch}", self.base, self.repo),
            )
            .header("Authorization", format!("token {}", self.token))
            .send()
            .await
            .with_context(|| format!("forge GET branches/{branch}"))?;
        let status = resp.status();
        if status == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let body = resp.text().await?;
        if !status.is_success() {
            bail!(
                "forge GET /repos/{}/branches/{branch}: HTTP {status}: {}",
                self.repo,
                body.trim()
            );
        }
        let v: Value = serde_json::from_str(&body)
            .with_context(|| format!("parsing forge branches/{branch} response"))?;
        Ok(v.get("commit")
            .and_then(|c| c.get("id"))
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string))
    }

    /// List the repo's recent runs, pick this train's still-active
    /// ones with `cancellable_run_ids`, and POST cancel to each.
    ///
    /// NEVER PROPAGATES. Cancelling CI is a courtesy to the next
    /// train; failing to do it must not abort the cancel that
    /// releases the cars, because a car stuck aboard a dead train is
    /// far worse than a run left burning. Every failure is logged and
    /// swallowed, and the count returned is what actually succeeded.
    async fn cancel_ci_runs(&self, pr_index: &str, head_sha: &str) -> Result<usize> {
        let listed = match self
            .api(
                Method::GET,
                &format!("/repos/{}/actions/runs?limit=50", self.repo),
                None,
            )
            .await
        {
            Ok(v) => v,
            Err(e) => {
                log(format!("cancel: could not list CI runs: {e}"));
                return Ok(0);
            }
        };
        let runs = listed
            .as_ref()
            .and_then(|v| v.get("workflow_runs"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let ids = cancellable_run_ids(&runs, pr_index, head_sha);
        let mut cancelled = 0;
        for id in ids {
            match self
                .api(
                    Method::POST,
                    &format!("/repos/{}/actions/runs/{id}/cancel", self.repo),
                    None,
                )
                .await
            {
                Ok(_) => {
                    cancelled += 1;
                    log(format!("cancel: cancelled CI run {id}"));
                }
                Err(e) => log(format!("cancel: CI run {id} would not cancel: {e}")),
            }
        }
        Ok(cancelled)
    }
}

fn make_forge(cfg: &Config) -> Result<Box<dyn Forge>> {
    match cfg.forge_kind.as_str() {
        "github" => Ok(Box::new(GitHubForge {
            head_owner: cfg.head_owner.clone(),
            fork_repo: repo_path(&cfg.fork_url),
        })),
        "forgejo" => Ok(Box::new(ForgejoForge::new()?)),
        other => bail!("unknown BOSS_TRAIN_FORGE {other:?} — expected github or forgejo"),
    }
}

/// Collapse the forge's per-check rollup to green/pending/failing.
/// The per-check detail behind a CI verdict, as `context:state`
/// pairs — the evidence `ci_verdict` reduces to a single word and
/// then discards.
///
/// David, 2026-08-17: "especially with agent actors, we want
/// verifiable evidence like a commit hash, actual CI pass report, or
/// other data that should already be getting generated as a result of
/// actually doing the work. This should be more about accounting and
/// documenting than needing a new step or capability."
///
/// This is exactly that: the rollup is already fetched to compute the
/// verdict, so recording it costs one string and no new call. Reading
/// a red train used to mean hand-querying the forge for the run and
/// then its jobs — three API shapes, none of them obvious — to learn
/// which check failed. Now the packet says.
fn ci_check_summary(rollup: Option<&Value>) -> String {
    let Some(items) = rollup.and_then(Value::as_array) else {
        return String::new();
    };
    items
        .iter()
        .map(|c| {
            let ctx = c
                .get("context")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .unwrap_or("?");
            let state = c
                .get("conclusion")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .or_else(|| c.get("status").and_then(Value::as_str))
                .filter(|s| !s.is_empty())
                .unwrap_or("?");
            format!("{ctx}:{state}")
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// The rollup read down to one word: `green`, `failing`, `aborted`
/// (a run was killed before it judged anything) or `pending`.
fn ci_verdict(rollup: Option<&Value>) -> &'static str {
    let Some(items) = rollup.and_then(Value::as_array).filter(|a| !a.is_empty()) else {
        return "pending";
    };
    let states: BTreeSet<String> = items
        .iter()
        .map(|c| {
            c.get("conclusion")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .or_else(|| {
                    c.get("status")
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                })
                .unwrap_or_default()
                .to_uppercase()
        })
        .collect();
    const FAILING: [&str; 3] = ["FAILURE", "TIMED_OUT", "ACTION_REQUIRED"];
    if states.iter().any(|s| FAILING.contains(&s.as_str())) {
        return "failing";
    }
    // A KILLED RUN JUDGED NOTHING. `CANCELLED` used to sit in FAILING,
    // which made an infrastructure incident indistinguishable from a
    // broken consist: on 2026-08-22 two runs died mid-flight, the
    // conductor read red, and four innocent cars took the strikes (see
    // `verdict_strikes_cars`). Ordered after the failing check on
    // purpose — a genuine failure beside a cancelled sibling is still a
    // real verdict, and the aborted sibling does not soften it.
    const SETTLED: [&str; 4] = ["SUCCESS", "NEUTRAL", "SKIPPED", "COMPLETED"];
    // A still-running sibling means the rollup has not settled — the
    // train may yet get an answer from it, cancelled neighbour or not.
    if states
        .iter()
        .any(|s| !SETTLED.contains(&s.as_str()) && s != "CANCELLED")
    {
        return "pending";
    }
    if states.iter().any(|s| s == "CANCELLED") {
        return "aborted";
    }
    "green"
}

// ---------------------------------------------------------------------------
// The conductor
// ---------------------------------------------------------------------------

struct Conductor {
    cfg: Config,
    http: reqwest::Client,
    forge: Box<dyn Forge>,
    /// THE RULES THIS INVOCATION DECIDES BY — resolved once, from the
    /// registry, and threaded to every decision point below. Nothing in
    /// this file reaches for a policy constant any more; if a threshold
    /// appears in a decision here, it arrived on this field.
    policy: DeliveryPolicy,
}

impl Conductor {
    fn new(cfg: Config, forge: Box<dyn Forge>) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .default_headers({
                // Machine token (7fcd78fa phase 1): rides as a default
                // header so every jobs-API verb the conductor runs
                // carries it once the operator configures one.
                let mut h = reqwest::header::HeaderMap::new();
                boss_core::machine_token::attach(&mut h);
                h
            })
            .build()?;
        // Built on the compiled fallback so the conductor can make the
        // very API call that resolves the real one; `with_policy`
        // replaces it before any decision is taken.
        Ok(Conductor {
            cfg,
            http,
            forge,
            policy: DeliveryPolicy::compiled(),
        })
    }

    fn with_policy(mut self, policy: DeliveryPolicy) -> Self {
        self.policy = policy;
        self
    }

    /// Read the delivery policy in force. Never fails — an unreachable
    /// or unusable registry lands on the compiled fallback with one loud
    /// journal line (`delivery_policy::resolve_from`), because a policy
    /// registry must not become a new way to wedge every train.
    async fn resolve_policy(&self) -> DeliveryPolicy {
        let fetched = self
            .api(
                Method::GET,
                &format!("/api/delivery/policy/{}", delivery_policy::POLICY_NAME),
                None,
            )
            .await
            .and_then(row_of_policy);
        let policy = delivery_policy::resolve_from(fetched, &|m| log(m));
        if policy.is_from_registry() {
            log(format!(
                "delivery policy v{} in force (hold {}, stall {}h, {} lint exclusions)",
                policy.version,
                policy.max_red_trains,
                policy.stall_hours,
                policy.excluded_lints.len()
            ));
        }
        policy
    }

    /// The policy a train in flight is judged by: the version it
    /// DEPARTED under, not the one in force now. A mid-flight registry
    /// edit changes the next train, never this one — the same promise a
    /// packet gets from its pinned workflow version.
    async fn policy_for(&self, train: &Value) -> DeliveryPolicy {
        let Some(version) = delivery_policy::version_to_fetch(train, &self.policy) else {
            return self.policy.clone();
        };
        let fetched = self
            .api(
                Method::GET,
                &format!(
                    "/api/delivery/policy/{}/versions/{version}",
                    delivery_policy::POLICY_NAME
                ),
                None,
            )
            .await
            .and_then(row_of_policy);
        match fetched.and_then(|row| {
            row.ok_or_else(|| anyhow!("policy v{version} is not in the registry"))
                .and_then(delivery_policy::parse)
        }) {
            Ok(pinned) => pinned,
            Err(e) => {
                // Loud, and then carry on under the active policy: a
                // train whose pin cannot be read still has to be
                // reconciled, and refusing would strand its consist.
                log(format!(
                    "delivery policy: train pinned v{version} but it could not be read ({e}) — \
                     judging it under v{} instead",
                    self.policy.version
                ));
                self.policy.clone()
            }
        }
    }

    /// Every jobs-API call the conductor makes, under the blip guard:
    /// a rolling system of record must not fail a whole verb.
    async fn api(
        &self,
        method: Method,
        path: &str,
        payload: Option<Value>,
    ) -> Result<Option<Value>> {
        retrying(
            &JOBS_API_RETRY,
            &method,
            self.policy.blip_cause_budget,
            &|m| log(m),
            || {
                let method = method.clone();
                let payload = payload.clone();
                async move { self.api_once(method, path, payload).await }
            },
        )
        .await
    }

    /// One attempt. Every way it can fail is classified on the way
    /// out, so the caller above decides retry-or-surface on evidence
    /// rather than on a string match over an error message.
    async fn api_once(
        &self,
        method: Method,
        path: &str,
        payload: Option<Value>,
    ) -> std::result::Result<Option<Value>, ApiFailure> {
        let mut req = self
            .http
            .request(method.clone(), format!("{}{path}", self.cfg.jobs))
            .header("content-type", "application/json")
            .header("x-boss-user", boss_user());
        if let Some(p) = &payload {
            req = req.json(p);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| ApiFailure::transport(e, format!("{method} {path}")))?;
        let status = resp.status();
        let body = resp
            .text()
            .await
            .map_err(|e| ApiFailure::transport(e, format!("reading {method} {path} response")))?;
        if !status.is_success() {
            return Err(ApiFailure {
                kind: Failure::Http(status.as_u16()),
                cause: anyhow!("{method} {path}: HTTP {status}: {}", body.trim()),
            });
        }
        if body.trim().is_empty() {
            return Ok(None);
        }
        serde_json::from_str(&body)
            .map(Some)
            .map_err(|e| ApiFailure {
                kind: Failure::Malformed,
                cause: anyhow::Error::new(e).context(format!("parsing {method} {path} response")),
            })
    }

    async fn get_job(&self, id: &str) -> Result<Value> {
        self.api(Method::GET, &format!("/api/jobs/{id}"), None)
            .await?
            .ok_or_else(|| anyhow!("job {id} came back empty"))
    }

    /// File the urgent packet a red train becomes — the estate-alarm
    /// idiom (kind backlog-item, priority urgent, on the pipeline
    /// subject), keyed by `train_alert` so the overdue/watchlist
    /// machinery can see it. Dedup is the train's `red_alert_filed`
    /// flag (see `announce_red_train`), not this key.
    async fn file_train_alert(&self, tid: &str, alert: &RedTrainAlert) -> Result<()> {
        self.api(
            Method::POST,
            "/api/jobs",
            Some(red_train_alert_body(tid, alert)),
        )
        .await?;
        Ok(())
    }

    /// Announce a red train unless it is already announced — best-effort
    /// caller in `reconcile`. Both the flag read and the POST are
    /// fallible; the caller treats ANY error here as non-fatal, because
    /// filing an alert is observability and must never abort the pass
    /// that boards, merges, and auto-cancels. See `reconcile`.
    ///
    /// Dedup is a per-train metadata FLAG (`red_alert_filed`), mirroring
    /// `deploy_alarm_filed` / `converge_alarm_filed` — not a scan of open
    /// backlog-items. The scan it replaces read `status=open&limit=200`
    /// and treated a truncated page as "no alert exists"; once open
    /// backlog-items passed 200 the existing alert sat beyond row 200,
    /// the dedup answered "not raised", and the alert re-filed every
    /// reconcile pass (a self-compounding notification flood). The flag
    /// is stamped only after a successful file, so a failed POST leaves
    /// the train unflagged and the next pass retries.
    async fn announce_red_train(&self, train: &Value, alert: &RedTrainAlert) -> Result<()> {
        if red_alert_filed(train) {
            return Ok(());
        }
        let tid = job_id(train)?;
        self.file_train_alert(tid, alert).await?;
        self.api(
            Method::PATCH,
            &format!("/api/jobs/{tid}/metadata"),
            Some(json!({"red_alert_filed": true})),
        )
        .await?;
        log(format!(
            "train {} red — filed alert: {}",
            id8(tid),
            alert.title
        ));
        Ok(())
    }

    /// Complete `step` on `job` with evidence fields (None values are
    /// dropped, matching the python kwargs filter).
    async fn complete_step(
        &self,
        job: &Value,
        step: Option<&Value>,
        fields: &[(&str, Option<String>)],
    ) -> Result<()> {
        if step_done(step) {
            return Ok(());
        }
        let jid = job_id(job)?;
        let step = step.ok_or_else(|| anyhow!("step missing on job {}", id8(jid)))?;
        let mut md = metadata_map(step);
        for (k, v) in fields {
            if let Some(v) = v {
                md.insert((*k).to_string(), json!(v));
            }
        }
        if self.cfg.dry {
            log(format!(
                "DRY: would complete {} on {} with {}",
                step_label(step),
                id8(jid),
                py_dict(fields)
            ));
            return Ok(());
        }
        let sid = step
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("step without an id on job {jid}"))?;
        // WHEN is evidence too: steps carry only a completion DATE,
        // so the conductor stamps the instant itself — the arrival
        // report's timings derive from these.
        md.insert(
            "completed_at".to_string(),
            json!(crate::gate::stamp(Utc::now())),
        );
        self.api(
            Method::PUT,
            &format!("/api/jobs/{jid}/steps/{sid}"),
            Some(json!({"status": "completed", "metadata": md})),
        )
        .await?;
        log(completion_log_line(
            &step_label(step),
            id8(jid).as_str(),
            fields,
        ));
        Ok(())
    }

    /// update_job takes a whole Job; fetch, merge metadata, put back.
    /// The overlay itself is `overlay_metadata` — pure, and pinned by
    /// tests: PUT replaces metadata wholesale, so clobbering here
    /// would silently eat another writer's keys. A `Value::Null`
    /// value removes the key.
    /// The server now offers this merge atomically as
    /// `PATCH /api/jobs/{id}/metadata` (same null-removes convention);
    /// migrating the conductor off this client-side RMW is a follow-up.
    async fn merge_job_metadata(&self, jid: &str, kv: Vec<(&str, Value)>) -> Result<Value> {
        let mut job = self.get_job(jid).await?;
        let keys: Vec<&str> = kv.iter().map(|(k, _)| *k).collect();
        let md = overlay_metadata(&job, kv);
        job["metadata"] = Value::Object(md);
        if self.cfg.dry {
            log(format!(
                "DRY: would set {} on job {}",
                py_keys(&keys),
                id8(jid)
            ));
            return Ok(job);
        }
        self.api(Method::PUT, &format!("/api/jobs/{jid}"), Some(job.clone()))
            .await?;
        Ok(job)
    }

    /// Close each boarded car of a just-merged train, BEST-EFFORT, and
    /// return how many failed to close this pass.
    ///
    /// Each car's close is its own fallible scope: a failure LOGS a line
    /// naming the car and the loop moves on, so one bad car cannot orphan
    /// the rest. The caller completes the train's `merged` step only when
    /// this returns 0, so a partial pass is retried on the next reconcile.
    /// All three writes are idempotent — `get_job` reads, `complete_step`
    /// early-returns on a done step, `merge_job_metadata` merges — so a
    /// retry re-closes only the car that did not close before.
    async fn close_boarded_cars(
        &self,
        tid: &str,
        boarded: &[String],
        merge_ref: &str,
        pr_url: &str,
    ) -> usize {
        let mut failures = 0usize;
        for cid in boarded {
            // The car's review closes HERE, not at boarding — the change
            // was open for review until it landed, and leaving the step
            // ready while the car rides is what lets a cancelled train
            // release it (see the boarding loop). Review first, because
            // the ship-a-change spec gates `merged` on `steps.review.done
            // AND job.metadata.merged`, and the marker below is what the
            // dispatcher watches to close the Job.
            let close: Result<()> = async {
                let car = self.get_job(cid).await?;
                let review = find_step(&car, "review", "Open for review");
                if !step_done(review) {
                    self.complete_step(
                        &car,
                        review,
                        &[
                            ("pr_url", Some(pr_url.to_string())),
                            ("note", Some(format!("landed on main as {merge_ref}"))),
                        ],
                    )
                    .await?;
                }
                // v3 ship-a-change gates `merged` on this marker; the
                // dispatcher closes the Job once it is set.
                self.merge_job_metadata(
                    cid,
                    vec![("merged", json!("true")), ("merge_ref", json!(merge_ref))],
                )
                .await?;
                Ok(())
            }
            .await;
            if let Err(e) = close {
                // BEST-EFFORT: one car's failed close must not orphan the
                // rest. Count it, name it, move on — the caller holds the
                // `merged` step pending so the next reconcile retries it.
                failures += 1;
                log(format!(
                    "train {} merged, but closing car {} failed (non-fatal, retries next \
                     pass): {e}",
                    id8(tid),
                    id8(cid)
                ));
            }
        }
        failures
    }

    // -----------------------------------------------------------------------
    // Phase 1 — reconcile open trains against reality
    // -----------------------------------------------------------------------

    /// Carry a merged train out to the playground — only from a clean
    /// main tree; anything else is recorded and retried next run.
    ///
    /// EMPTY-TREE CONTRACT. When `deploy_tree` is empty
    /// (`BOSS_TRAIN_DEPLOY_TREE=""`) the deploy happens ELSEWHERE, not
    /// here: the cluster converges on forge main by itself via the
    /// forge-host cluster-deploy-runner (deployment-as-network). This
    /// is the deliberate config for a conductor running inside the
    /// cluster, which has no `/opt/boss` tree and no sudo — the
    /// migration in docs/design/the-cluster-is-the-system.md, which
    /// retires the vestigial boss-gcp playground deploy. In that mode
    /// deploy() does no git or tree access at all: it completes the
    /// `deployed` step honestly (nothing to deploy) and returns, and
    /// the downstream convergence-verification step is what proves the
    /// cluster actually took the merge. The default stays `/opt/boss`,
    /// so the boss-gcp conductor's path is byte-unchanged.
    async fn deploy(&self, train: &Value, deployed_step: &Value, now: DateTime<Utc>) -> Result<()> {
        // Cluster-resident conductor: no playground deploy. Short-
        // circuit BEFORE any git/tree access — there is no tree, and a
        // no-op deploy has no business touching one. This is a
        // COMPLETION, not a block (see NO_PLAYGROUND_DEPLOY_EVIDENCE):
        // convergence verification downstream confirms the merge landed.
        if playground_deploy_disabled(&self.cfg.deploy_tree) {
            log("deploy skipped — no playground tree; the cluster converges on forge main");
            self.complete_step(
                train,
                Some(deployed_step),
                &[("deployed", Some(NO_PLAYGROUND_DEPLOY_EVIDENCE.to_string()))],
            )
            .await?;
            return Ok(());
        }
        let tree = self.cfg.deploy_tree.clone();
        let tree_path = Path::new(&tree);
        // Deploy only when needed. The skip decision comes before the
        // busy check — a no-op deploy has no business caring about
        // the tree — and reads two facts: the generation store's live
        // key and what `main` is on the remote. Matching pair: record
        // the evidence on the step and journal the skip; the services
        // stay unbounced.
        let pull_remote = env_or("BOSS_TRAIN_DEPLOY_REMOTE", "origin");
        let remote_out = sh_unchecked(&["git", "-C", &tree, "ls-remote", &pull_remote, "main"])?;
        let remote_main = stdout_str(&remote_out)
            .split_whitespace()
            .next()
            .unwrap_or_default()
            .to_string();
        let current = current_generation_key();
        if !deploy_needed(&current, &remote_main) {
            let short: String = remote_main.chars().take(12).collect();
            log(format!(
                "deploy skipped — generation {current} already serves main@{short}"
            ));
            self.complete_step(
                train,
                Some(deployed_step),
                &[(
                    "deployed",
                    Some(format!(
                        "already live: generation {current} serves main@{short}; no deploy run"
                    )),
                )],
            )
            .await?;
            return Ok(());
        }
        let dirty_out = sh_unchecked(&["git", "-C", &tree, "status", "--porcelain"])?;
        let dirty = !stdout_str(&dirty_out).trim().is_empty();
        let branch_out = sh(&["git", "-C", &tree, "rev-parse", "--abbrev-ref", "HEAD"])?;
        let branch = stdout_str(&branch_out).trim().to_string();
        if dirty || branch != "main" {
            // dirty prints True/False — python's bool repr; the journal
            // line is operator surface and stays byte-identical.
            let reason = format!(
                "deploy tree busy (branch={branch}, dirty={}) — will retry",
                if dirty { "True" } else { "False" }
            );
            log(&reason);
            if !self.cfg.dry {
                let tid = job_id(train)?;
                let sid = deployed_step
                    .get("id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow!("deployed step without an id on job {tid}"))?;
                let mut md = metadata_map(deployed_step);
                md.insert("deploy_blocked".to_string(), json!(reason));
                // WHEN the block started, stamped once and left alone
                // while it persists — the elapsed time is the whole
                // signal, so a stamp that refreshed every pass would
                // make an indefinite block look permanently fresh.
                let blocked_since = md
                    .get("deploy_blocked_since")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .unwrap_or_else(|| now.to_rfc3339());
                md.insert("deploy_blocked_since".to_string(), json!(blocked_since));
                self.api(
                    Method::PUT,
                    &format!("/api/jobs/{tid}/steps/{sid}"),
                    Some(json!({"metadata": md})),
                )
                .await?;

                let since = parse_stamp(Some(blocked_since.as_str()));
                if deploy_block_verdict(since, now, self.cfg.converge_alarm_mins)
                    == DeployBlockVerdict::Overdue
                    && !truthy(
                        train
                            .get("metadata")
                            .and_then(|m| m.get("deploy_alarm_filed")),
                    )
                {
                    let mins = since
                        .map(|s| (now.fixed_offset() - s).num_minutes())
                        .unwrap_or_default();
                    log(format!(
                        "train {}: deploy tree BLOCKED {mins} min — filing packet",
                        id8(tid)
                    ));
                    self.api(
                        Method::POST,
                        "/api/jobs",
                        Some(json!({
                            "kind": "user-feedback",
                            "status": "open",
                            "title": format!(
                                "Deploy blocked {mins} min: the playground tree is not clean"
                            ),
                            "subject": {"subject_kind": "custom", "id": "cluster-convergence"},
                            "tags": ["deploy", "pipeline"],
                            "owner_id": "emp-david",
                            "priority": "urgent",
                            "opened_on": now.date_naive().to_string(),
                            "metadata": {
                                "message": format!(
                                    "The conductor has refused to deploy for {mins} minutes: \
                                     {reason}. Refusing is correct — building from an unknown \
                                     working state is worse than waiting — but waiting SILENTLY \
                                     is the defect this packet exists to end (2026-09-02: a \
                                     regenerated Cargo.lock left the tree dirty and two merged \
                                     trains waited six hours while the retry logged to nobody). \
                                     Inspect with `git -C <deploy tree> status --short`; a \
                                     regenerable artifact is `git checkout --` and the next tick \
                                     deploys. Threshold is BOSS_TRAIN_CONVERGE_ALARM_MINS ({}).",
                                    self.cfg.converge_alarm_mins
                                ),
                                "train": tid,
                                "blocked_since": blocked_since,
                            },
                        })),
                    )
                    .await?;
                    self.api(
                        Method::PATCH,
                        &format!("/api/jobs/{tid}/metadata"),
                        Some(json!({"deploy_alarm_filed": true})),
                    )
                    .await?;
                }
            }
            return Ok(());
        }
        if self.cfg.dry {
            log("DRY: would pull main, migrate, build, deploy services + web");
            return Ok(());
        }
        // Under the forge protocol the playground converges on forge
        // main; GitHub is the mirror, never the source (27ab7680).
        sh(&["git", "-C", &tree, "pull", &pull_remote, "main"])?;
        let main_ref_out = sh(&["git", "-C", &tree, "rev-parse", "--short", "HEAD"])?;
        let main_ref = stdout_str(&main_ref_out).trim().to_string();
        let mig = Command::new(format!("{tree}/infra/postgres/migrate.sh"))
            .args(["--", "psql", "-U", "boss", "-h", "127.0.0.1", "-d", "boss"])
            .current_dir(tree_path)
            .env("PGPASSWORD", "boss")
            .output()
            .context("spawning migrate.sh")?;
        if !mig.status.success() {
            bail!(
                "migrate.sh failed:\n{}",
                String::from_utf8_lossy(&mig.stderr).trim()
            );
        }
        sh_in(
            Some(tree_path),
            true,
            &[&format!("{tree}/infra/build-release.sh")],
        )?;
        sh_in(
            Some(tree_path),
            true,
            &[
                "sudo",
                "-n",
                &format!("{tree}/infra/deploy-services.sh"),
                "prod",
            ],
        )?;
        sh_in(
            Some(tree_path),
            true,
            &["sudo", "-n", &format!("{tree}/infra/deploy-web.sh")],
        )?;
        let mig_out = stdout_str(&mig);
        let summary = format!(
            "main@{main_ref}; {}; services: prod; web: deployed",
            mig_out.trim().lines().last().unwrap_or_default()
        );
        self.complete_step(train, Some(deployed_step), &[("deployed", Some(summary))])
            .await?;
        Ok(())
    }

    /// Verify the CLUSTER is serving this train's merge, and complete
    /// the `converged` step with the evidence — or file the loud
    /// packet when convergence has lagged past the threshold.
    ///
    /// The proof is self-report: the jobs API's health endpoint
    /// answers with the commit its binary was BUILT from
    /// (`Capabilities.commit`, baked in by the image build). That is
    /// stronger than reading the image tag off the Deployment — a tag
    /// proves a push was requested; a running binary reporting the
    /// commit proves the pod restarted onto it.
    async fn verify_convergence(&self, train: &Value, now: DateTime<Utc>) -> Result<()> {
        let tid = job_id(train)?.to_string();
        let converged_step = find_step(train, "converged", "Cluster converged")
            .ok_or_else(|| anyhow!("converged step missing on job {}", id8(&tid)))?;
        let merge_ref = find_step(train, "merged", "Merged into main")
            .and_then(|s| s.get("metadata"))
            .and_then(|m| m.get("merge_ref"))
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("no merge_ref on job {} — nothing to verify", id8(&tid)))?
            .to_string();
        let health = self.api(Method::GET, "/api/jobs/health", None).await?;
        let cluster_commit = health
            .as_ref()
            .and_then(|h| h.get("capabilities"))
            .and_then(|c| c.get("commit"))
            .and_then(Value::as_str)
            .map(str::to_string);
        let merged_at = parse_stamp(step_stamp(train, "merged", "Merged into main"));
        let mins_since_merge = merged_at
            .map(|m| (now.fixed_offset() - m).num_minutes())
            .unwrap_or(0);
        // Equality misses "rolled past" (see convergence_verdict); ask
        // git the ancestry question only when equality already failed,
        // against the conductor clone — which reconcile keeps fetched.
        // Any git failure (no clone yet, commit unknown to the forge)
        // reads None: converges nothing, never guesses.
        let ancestor = match cluster_commit.as_deref() {
            Some(c) if !commits_match(&merge_ref, c) => {
                let clone = self.cfg.clone.clone();
                sh_unchecked(&[
                    "git",
                    "-C",
                    &clone,
                    "merge-base",
                    "--is-ancestor",
                    &merge_ref,
                    c,
                ])
                .ok()
                .map(|o| o.status.success())
            }
            _ => None,
        };
        match convergence_verdict(
            &merge_ref,
            cluster_commit.as_deref(),
            ancestor,
            mins_since_merge,
            self.cfg.converge_alarm_mins,
        ) {
            ConvergenceVerdict::Converged => {
                let commit = cluster_commit.unwrap_or_default();
                self.complete_step(
                    train,
                    Some(converged_step),
                    &[
                        ("cluster_commit", Some(commit.clone())),
                        (
                            "verified",
                            Some(format!(
                                "the running cluster jobs API self-reports build commit \
                                 {} — matches merge {merge_ref}; verified {} min after merge",
                                id8(&commit),
                                mins_since_merge
                            )),
                        ),
                    ],
                )
                .await
            }
            ConvergenceVerdict::Waiting => {
                log(format!(
                    "train {}: cluster not yet on {} ({} min since merge, alarm at {})",
                    id8(&tid),
                    id8(&merge_ref),
                    mins_since_merge,
                    self.cfg.converge_alarm_mins
                ));
                Ok(())
            }
            ConvergenceVerdict::Overdue => {
                if truthy(
                    train
                        .get("metadata")
                        .and_then(|m| m.get("converge_alarm_filed")),
                ) {
                    return Ok(());
                }
                log(format!(
                    "train {}: cluster convergence OVERDUE ({} min since merge) — filing packet",
                    id8(&tid),
                    mins_since_merge
                ));
                if self.cfg.dry {
                    return Ok(());
                }
                let reported = cluster_commit.as_deref().unwrap_or("nothing");
                self.api(
                    Method::POST,
                    "/api/jobs",
                    Some(json!({
                        "kind": "user-feedback",
                        "status": "open",
                        "title": format!(
                            "Cluster convergence overdue: train {} merged {} min ago",
                            id8(&tid), mins_since_merge
                        ),
                        "subject": {"subject_kind": "custom", "id": "cluster-convergence"},
                        "tags": ["deploy", "pipeline"],
                        "owner_id": "emp-david",
                        "priority": "urgent",
                        "opened_on": now.date_naive().to_string(),
                        "metadata": {
                            "message": format!(
                                "The train merged {merge_ref} {mins_since_merge} minutes ago and \
                                 the cluster's running binary still reports {reported} — past the \
                                 {}-minute threshold (BOSS_TRAIN_CONVERGE_ALARM_MINS). Filed by \
                                 the conductor's converged step (fdff316c / 7e5ee013): the likely \
                                 suspects are the deploy-runner timer on the forge host, the image \
                                 build failing, or the rollout wedged — check \
                                 cluster-deploy-runner's journal first. The train's arrival report \
                                 will not fire until convergence verifies.",
                                self.cfg.converge_alarm_mins
                            ),
                            "train": tid,
                        },
                    })),
                )
                .await?;
                self.merge_job_metadata(&tid, vec![("converge_alarm_filed", json!(true))])
                    .await?;
                Ok(())
            }
        }
    }

    // `record_abandon_reason` lived here: it wrote the machine's reason
    // onto the `cancelled` terminal of a train the board had opened only
    // to abandon. A board no longer opens a packet it is not departing
    // (see "A BOARD THAT DEPARTS NO TRAIN OPENS NO PACKET"), so there is
    // no self-cancelled train left to explain — the reason it used to
    // carry is now the journal's `no train departed` line and the cars'
    // own `skip_reason`. An operator's `boss train cancel` still fills
    // the same terminal with its `--reason`, on its own path.

    /// Settle gate-runs whose runner died without reporting: complete
    /// `record-verdict` as `lost`, the terminal the workflow already
    /// provides for exactly this. NOT green and NOT failed — the checks
    /// never finished, so the run says nothing about the branch, and a
    /// verdict nobody observed would be a lie the audit log carries
    /// forever. The decision itself is `dead_gate_run_hours`, pure and
    /// tested; this is the adapter that acts on it.
    async fn reap_dead_gate_runs(&self, now: DateTime<Utc>) -> Result<()> {
        let runs = rows(
            self.api(
                Method::GET,
                "/api/jobs?kind=gate-run&status=open&limit=100",
                None,
            )
            .await?,
        )?;
        for r0 in runs {
            let rid = job_id(&r0)?.to_string();
            let run = self.get_job(&rid).await?;
            let Some(hours) = dead_gate_run_hours(&run, now) else {
                continue;
            };
            let branch = metadata_map(&run)
                .get("branch")
                .and_then(Value::as_str)
                .unwrap_or("(no branch)")
                .to_string();
            log(format!(
                "reconcile: gate-run {} ({branch}) active {hours}h with no verdict — \
                 past the {GATE_DEADLINE_HOURS}h Job deadline, settling as lost",
                id8(&rid)
            ));
            let verdict_step = find_step(&run, "record-verdict", "Record the gate verdict");
            self.complete_step(
                &run,
                verdict_step,
                &[
                    ("verdict", Some("lost".to_string())),
                    (
                        "receipt",
                        Some(format!(
                            "NO VERDICT WAS PRODUCED. Active {hours}h with none recorded, past \
                             the gate Job's {GATE_DEADLINE_HOURS}h activeDeadlineSeconds, so the \
                             pod is gone and the checks never finished. Settled as LOST by the \
                             conductor's reconcile: this run says nothing about {branch}, and an \
                             infrastructure death is not a consist failure. Re-gate for a real \
                             verdict."
                        )),
                    ),
                ],
            )
            .await?;
        }
        Ok(())
    }

    /// A change that landed buries its own verdicts. A closed gate-run
    /// whose verdict was `failed` or `lost` stays a red row on the yard's
    /// approach until a car names its branch, a later green answers it,
    /// or an operator annotates it `superseded` — and a change that went
    /// to main through the emergency lane has none of those, so its dead
    /// gate sat red on the yard for a day (fix/lean-ci-builds, lost
    /// 2026-09-04, buried by hand 2026-09-05). This is the machine's
    /// version of that annotation: if the run's sha is already an
    /// ancestor of main, the question it raised is answered by main
    /// itself. `verdict_to_bury` decides, pure and tested; this is the
    /// adapter that asks git and writes the annotation the yard reads.
    async fn bury_landed_verdicts(&self, now: DateTime<Utc>) -> Result<()> {
        let runs = rows(
            self.api(
                Method::GET,
                "/api/jobs?kind=gate-run&status=closed&limit=100",
                None,
            )
            .await?,
        )?;
        let clone = self.cfg.clone.clone();
        for run in runs {
            let Some((sha, verdict)) = verdict_to_bury(&run, now) else {
                continue;
            };
            let landed = sh_unchecked(&[
                "git",
                "-C",
                &clone,
                "merge-base",
                "--is-ancestor",
                &sha,
                "origin/main",
            ])?
            .status
            .success();
            if !landed {
                continue;
            }
            let main_sha = stdout_str(&sh_unchecked(&[
                "git",
                "-C",
                &clone,
                "rev-parse",
                "--short",
                "origin/main",
            ])?)
            .trim()
            .to_string();
            let rid = job_id(&run)?.to_string();
            let branch = metadata_map(&run)
                .get("branch")
                .and_then(Value::as_str)
                .unwrap_or("(no branch)")
                .to_string();
            log(format!(
                "reconcile: gate-run {} ({branch}) went {verdict}, but its sha {} is an ancestor of main ({main_sha}) — the change landed; burying the verdict",
                id8(&rid),
                &sha[..sha.len().min(7)]
            ));
            self.merge_job_metadata(
                &rid,
                vec![(
                    "superseded",
                    json!(format!(
                        "landed on main: {} is an ancestor of {main_sha} — the change went in without this gate (a re-gate or the emergency lane); buried by the conductor's reconcile",
                        &sha[..sha.len().min(7)]
                    )),
                )],
            )
            .await?;
        }
        Ok(())
    }

    async fn reconcile(&self, now: DateTime<Utc>) -> Result<()> {
        // Keep the clone fetched before the convergence check below asks git
        // "is the cluster's running commit a descendant of this train's
        // merge?" — a question answered AGAINST THIS CLONE. reconcile does
        // not board (only boarding called ensure_clone), so without this the
        // clone stays frozen at the last board and lacks the cluster's newer
        // commit; merge-base then exits non-zero (object unknown), is read as
        // "not an ancestor", and every train whose commit was superseded
        // between boards wedges at `converged` forever. On 2026-09-04 three
        // trains wedged exactly this way after the cutover boarded once and
        // then reconciled repeatedly against a stale clone. convergence_verdict's
        // comment claimed reconcile kept the clone fetched; it did not until
        // this line. A fetch failure is non-fatal — ancestry falls to None,
        // which converges nothing and retries next pass, the safe direction.
        // Log a failure rather than swallow it: a silent ensure_clone
        // error (the .ok() this replaces) is exactly how a broken clone
        // stayed invisible while trains wedged.
        if let Err(e) = self.ensure_clone() {
            log(format!(
                "reconcile: ensure_clone failed — ancestry-based convergence \
                 reads None this pass (converges nothing, retries): {e}"
            ));
        }
        // Bury the yard's dead before reading it. A gate pod that dies
        // without recording a verdict leaves its packet at
        // `record-verdict` forever: it holds one of the three gate slots,
        // renders as a live gate, and nothing ever clears it. On
        // 2026-09-04 two such ghosts sat there 17 hours with their
        // branches long landed, and a third silently ate a car — the
        // change was never gated and nobody noticed until a census.
        //
        // gate-runner.yaml already states the intent ("a runner that dies
        // anyway leaves an overdue packet — the alarm the protocol
        // already provides"); the alarm just had nobody listening. This
        // is the listener, and it belongs in reconcile because reconcile
        // IS the verb that makes the record match reality.
        //
        // Settling requires no cluster access, only a clock: past the
        // gate Job's own activeDeadlineSeconds, Kubernetes has already
        // killed the Job, so a packet still claiming to gate cannot be.
        if let Err(e) = self.reap_dead_gate_runs(now).await {
            log(format!("reconcile: gate-run reap failed (non-fatal): {e}"));
        }
        if let Err(e) = self.bury_landed_verdicts(now).await {
            log(format!(
                "reconcile: burying landed verdicts failed this pass (retries next): {e}"
            ));
        }
        let trains = rows(
            self.api(
                Method::GET,
                "/api/jobs?kind=pr-train&status=open&limit=50",
                None,
            )
            .await?,
        )?;
        let trains_len = trains.len();
        let mut isolated_failures = 0usize;
        for t0 in trains {
            // PER-TRAIN ISOLATION (2026-09-06). One train's failure — a
            // failed observability write, a forge blip, a merge conflict —
            // must never abort the pass and wedge every train behind it.
            // That is what froze all landings for ~8h: a red-train alert
            // POST returned 422 and, filed with `?`, aborted reconcile
            // every pass. Each iteration now runs in its own fallible
            // scope, so a sick train costs itself one pass, not the fleet.
            // (`continue` inside the loop body therefore becomes
            // `return Ok(())` — the same "skip the rest of this train".)
            let outcome: Result<()> = async {
                let tid = job_id(&t0)?.to_string();
                let mut t = self.get_job(&tid).await?;
            // The rules THIS train departed under, which may not be the
            // ones in force now.
            let policy = self.policy_for(&t).await;
            // The stall sentinel first — a train stuck BEFORE its PR
            // (assembly died, push hung) would slip past the
            // pr-step early-continues below and stall invisibly.
            self.note_stall(&t, now, &policy).await?;
            let pr_step = find_step(&t, "pr", "Open the batched PR");
            if !step_done(pr_step) {
                return Ok(()); // this window's board phase, or a stalled assembly
            }
            let pr_url = pr_step
                .and_then(|s| s.get("metadata"))
                .and_then(|m| m.get("pr_url"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            if pr_url.is_empty() {
                return Ok(());
            }
            let mut info = self.forge.pr_info(&pr_url).await?;

            let ci_step = find_step(&t, "ci", "CI verdict");
            let verdict = ci_verdict(info.get("statusCheckRollup"));
            if !step_done(ci_step) && verdict != "pending" {
                let checks = ci_check_summary(info.get("statusCheckRollup"));
                // WHY, not just WHICH: the tail of each failing job's log,
                // resolved through /actions/runs/{run}/jobs (empty on green
                // or when no log could be fetched).
                let check_logs = format_check_logs(
                    &failing_check_logs(info.get("statusCheckRollup")),
                    CI_STEP_LOG_BYTES,
                );
                self.complete_step(
                    &t,
                    ci_step,
                    &[
                        ("result", Some(verdict.to_string())),
                        // WHICH check, not just that one failed.
                        ("checks", (!checks.is_empty()).then_some(checks)),
                        // The failing job's log tail — a verdict names WHY.
                        ("check_logs", (!check_logs.is_empty()).then_some(check_logs)),
                    ],
                )
                .await?;
            } else if step_done(ci_step) {
                // The step has already recorded its verdict and cannot
                // record another — terminal rows are frozen. Compare
                // against the last verdict we NOTICED (the job stamp,
                // falling back to the step's original) so this fires on
                // each change rather than on every ten-minute tick.
                let md = t.get("metadata");
                let noticed = md
                    .and_then(|m| m.get("ci_verdict_latest"))
                    .and_then(Value::as_str)
                    .or_else(|| {
                        ci_step
                            .and_then(|s| s.get("metadata"))
                            .and_then(|m| m.get("result"))
                            .and_then(Value::as_str)
                    });
                if let Some(note) = verdict_drift(noticed, verdict) {
                    log(format!("train {}: {note}", id8(&tid)));
                    if !self.cfg.dry {
                        self.merge_job_metadata(
                            &tid,
                            vec![
                                ("ci_verdict_latest", json!(verdict)),
                                ("ci_verdict_changed_at", json!(now.to_rfc3339())),
                            ],
                        )
                        .await?;
                        t = self.get_job(&tid).await?;
                    }
                }
            }

            // Asked and unanswered. Stamped once, like the stall
            // sentinel, so a hung runner produces one line rather than
            // one every ten minutes for as long as it hangs.
            if !truthy(t.get("metadata").and_then(|m| m.get("ci_overdue_since")))
                && let Some(note) = ci_overdue(&t, now, self.cfg.ci_hours)
            {
                log(format!("train {}: {note}", id8(&tid)));
                if !self.cfg.dry {
                    self.merge_job_metadata(
                        &tid,
                        vec![("ci_overdue_since", json!(now.to_rfc3339()))],
                    )
                    .await?;
                    t = self.get_job(&tid).await?;
                }
            }

            // The overnight rule, before the merge check: a train that
            // is red — or whose run was killed without judging anything
            // — AND has stopped moving releases its consist so the next
            // window can board without it. Decided on the LIVE verdict
            // just read, never on the `ci` step's first stamp. Whether
            // the release counts against the cars is a separate
            // question, and only a returned failing verdict answers it
            // yes (`verdict_strikes_cars`).
            // A red train announces ITSELF, immediately — not only when it
            // stalls out into auto-cancel below, and not only when a human
            // asks. One urgent packet naming the failing check, deduped, so
            // a red train is never a surprise (d69c4274).
            //
            // BEST-EFFORT, and that is load-bearing: filing this alert is
            // observability, and observability must NEVER abort the pass
            // that boards, merges, and auto-cancels. The first cut filed it
            // with `?`, so a malformed body (HTTP 422) aborted reconcile at
            // rc=1 every pass — one red train froze all landings for ~8h
            // (2026-09-06). Any error here now logs and the pass continues,
            // so a broken alert is at worst a missing alert, never a wedge.
            if info.get("state").and_then(Value::as_str) == Some("OPEN")
                && let Some(alert) = red_train_alert(&t, verdict, info.get("statusCheckRollup"))
            {
                if self.cfg.dry {
                    log(format!(
                        "DRY: would alert on red train {} ({})",
                        id8(&tid),
                        alert.title
                    ));
                } else if let Err(e) = self.announce_red_train(&t, &alert).await {
                    log(format!(
                        "train {} red — alert filing failed (non-fatal, reconcile continues): {e}",
                        id8(&tid)
                    ));
                }
            }

            // The yard's cancel button (7a24caf3): an operator's
            // `cancel_requested` stamp is honoured before the automatic
            // rule and never strikes the cars. Non-fatal by construction
            // — the method returns a bool, so nothing here can abort the
            // pass — and a request claims the train's pass whether the
            // cancel succeeded, is dry, or is being retried.
            if self
                .honour_cancel_request(&t, &tid, info.get("state").and_then(Value::as_str))
                .await
            {
                return Ok(());
            }

            if self.cfg.auto_cancel
                && info.get("state").and_then(Value::as_str) == Some("OPEN")
                && let Some(reason) = auto_cancel_reason(&t, verdict, now, policy.stall_hours)
            {
                log(format!("train {} auto-cancelling: {reason}", id8(&tid)));
                if self.cfg.dry {
                    log(format!("DRY: would cancel {} ({reason})", id8(&tid)));
                } else {
                    self.cancel_train(
                        &tid,
                        &reason,
                        verdict_strikes_cars(verdict, info.get("statusCheckRollup")),
                    )
                    .await?;
                }
                return Ok(());
            }

            let pr_state = info.get("state").and_then(Value::as_str);
            if self.cfg.auto_merge && verdict == "green" && pr_state == Some("OPEN") {
                log(format!(
                    "CI green — merging {pr_url} (train protocol 27ab7680)"
                ));
                if !self.cfg.dry {
                    self.forge.merge(&pr_url).await?;
                    info = self.forge.pr_info(&pr_url).await?;
                }
            } else if let Some(why) = merge_declined_reason(self.cfg.auto_merge, verdict, pr_state)
            {
                // A decline says so. Silence here cost 2026-09-04 hours —
                // see `merge_declined_reason`. Not stamped-once like the
                // overdue sentinel: green-and-unmerged is a train stopped
                // one step from landing, and it should read as stopped on
                // every pass until the switch is on or the operator merges.
                log(format!("CI green on {pr_url} but NOT merging — {why}"));
            }

            let merged_step = find_step(&t, "merged", "Merged into main");
            if info.get("state").and_then(Value::as_str) == Some("MERGED")
                && !step_done(merged_step)
            {
                let merge_ref: String = info
                    .get("mergeCommit")
                    .and_then(|m| m.get("oid"))
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
                    .chars()
                    .take(12)
                    .collect();
                let boarded: Vec<String> = t
                    .get("metadata")
                    .and_then(|m| m.get("boarded_jobs"))
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(|v| v.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default();
                // Close every boarded car BEST-EFFORT, then complete the
                // train's `merged` step — and only when nothing failed.
                //
                // ORDER IS LOAD-BEARING. The first cut completed `merged`
                // FIRST and looped the cars with `?`: one car whose close
                // write errored aborted the (isolated) per-train scope, but
                // `merged` was already `completed`, so the retry guard above
                // (`state==MERGED && !step_done(merged)`) was false forever
                // after — the OTHER landed cars never got their close
                // markers and their car Jobs stayed open as residue,
                // inflating the open-car count and starving boarding. Now a
                // bad car costs only itself, and a partial pass leaves
                // `merged` pending so the next reconcile retries the
                // stragglers. Every close write is idempotent, so the retry
                // re-closes only the car that did not close before.
                let failures = self
                    .close_boarded_cars(&tid, &boarded, &merge_ref, &pr_url)
                    .await;
                if failures == 0 {
                    self.complete_step(&t, merged_step, &[("merge_ref", Some(merge_ref.clone()))])
                        .await?;
                } else {
                    log(format!(
                        "train {} merged, but {failures} car(s) failed to close this pass — \
                         holding the `merged` step pending so the next reconcile retries them",
                        id8(&tid)
                    ));
                }
                t = self.get_job(&tid).await?;
            }

            let merged_step = find_step(&t, "merged", "Merged into main");
            let deployed_step = find_step(&t, "deployed", "Deployed to the playground");
            if step_done(merged_step) && !step_done(deployed_step) {
                let deployed_step = deployed_step
                    .ok_or_else(|| anyhow!("deployed step missing on job {}", id8(&tid)))?;
                self.deploy(&t, deployed_step, now).await?;
                t = self.get_job(&tid).await?;
            }
            // Installation is not the finish line either — the cluster
            // must be SERVING the merge before the train can claim
            // arrival (fdff316c / 7e5ee013, decided 2026-08-19).
            // Trains admitted under the pre-converged spec have no
            // such step and skip this whole pass — version pinning
            // working as designed, nothing stranded.
            let converged_step = find_step(&t, "converged", "Cluster converged");
            if step_done(find_step(&t, "deployed", "Deployed to the playground"))
                && converged_step.is_some()
                && !step_done(converged_step)
                && let Err(e) = self.verify_convergence(&t, now).await
            {
                // Convergence checking must not fail the run whose
                // deploys succeeded — the next pass looks again, and
                // the overdue alarm bounds the silence.
                log(format!("convergence check failed (run stands): {e}"));
            }
                Ok(())
            }
            .await;
            if let Err(e) = outcome {
                isolated_failures += 1;
                let tid = t0
                    .get("id")
                    .and_then(Value::as_str)
                    .map(id8)
                    .unwrap_or_else(|| "?".to_string());
                log(format!(
                    "reconcile: train {tid} failed this pass — isolated, other trains continue: {e}"
                ));
            }
        }
        // Isolation must not become a blind spot. If EVERY train failed
        // this pass, that is almost never N independent per-train faults —
        // it is a systemic outage (forge / API / auth) that per-train
        // logging would scatter into noise indistinguishable from an
        // all-green pass. Say so, loudly and once, so a total downstream
        // failure surfaces rather than hiding behind the very isolation
        // that protects against the single-bad-train case.
        if trains_len > 0 && isolated_failures == trains_len {
            log(format!(
                "reconcile: ALL {trains_len} train(s) failed this pass — likely a SYSTEMIC \
                 outage (forge/API/auth), not per-train faults; investigate"
            ));
        }
        // Housekeeping must not fail a run whose real work succeeded.
        // The sweep runs last, after merges, deploys and evidence are
        // recorded; on 2026-08-13 a single un-deletable branch made
        // every reconcile report rc=1 and re-file its arrival report,
        // which reads as "the conductor is broken" when the trains had
        // in fact landed. Journal the failure, keep the verb green.
        if let Err(e) = self.sweep_landed_branches().await {
            log(format!(
                "branch sweep failed (housekeeping, run stands): {e}"
            ));
        }
        // The dock's merge preview rides the same tick (12a25f3e):
        // best-effort like the sweep — a failed preview journals and
        // the reconcile stands, because a projection that sometimes
        // lags is stale-not-wrong by design.
        if let Err(e) = self.preview_dock(now).await {
            log(format!("dock preview failed (projection, run stands): {e}"));
        }
        // A stranded green — gated, never parked — rots silently until a
        // human runs orient and reads it. This makes that detection
        // active: a green past the threshold with no car files a
        // backlog-item so the overdue/watchlist machinery can see it.
        // BEST-EFFORT, like the sweep and preview above: this froze
        // delivery for 8h once (a fatal write in reconcile stops ALL
        // landings — boss-conductor-loop-writes-must-not-be-fatal), so
        // any failure — a bad read, the POST filing the packet — journals
        // one visible line and the reconcile stands. Never .ok()-swallow:
        // a silent failure here is exactly how a strand stays unfiled.
        if let Err(e) = self.alarm_stranded_greens(now).await {
            log(format!(
                "stranded-green alarm failed (housekeeping, run stands): {e}"
            ));
        }
        Ok(())
    }

    /// File a best-effort backlog-item for each stranded green past its
    /// window that no open alarm already names, REFRESH the alarm of a
    /// strand that persists, and CLOSE the alarm of a strand that ended.
    /// Detection is `census::stranded_gate_runs` (which reads
    /// `boss_jobs::stranded`, the one definition the yard uses, §9a);
    /// the pure selection + dedup is `stranded_greens_to_alarm`; this
    /// method is only the I/O around them. Reads the same closed
    /// gate-runs orient reads (a gate-run CLOSES on its verdict, so a
    /// `status=open` query would miss every green). Returns `Err` on a
    /// read/write failure so the caller can journal it — the caller
    /// wraps this non-fatally.
    ///
    /// THE CLEAR IS NOT OPTIONAL. Raising and never revisiting the
    /// claim left four false STRANDED GREEN packets on the operator's
    /// queue on 2026-09-09, closed by hand (e60398dc): each branch
    /// parked, landed or was held within minutes of the alarm.
    async fn alarm_stranded_greens(&self, now: DateTime<Utc>) -> Result<()> {
        let gate_runs = rows(
            self.api(Method::GET, "/api/jobs?kind=gate-run&limit=60", None)
                .await?,
        )?;
        // EVERY car, not one page: at 832 ship-a-change packets on
        // 2026-09-09 the old `limit=800` was already truncated, and the
        // rows it dropped were the OLDEST — so a landed branch whose car
        // had aged off the page read as "no car" and alarmed
        // (a-limit-is-not-a-filter). Open AND closed: a landing closes
        // the car, and a closed car still answers "this branch has one".
        let cars = list_all_pages(|offset| async move {
            self.api(
                Method::GET,
                &format!("/api/jobs?kind=ship-a-change&limit={PAGE_LIMIT}&offset={offset}"),
                None,
            )
            .await
        })
        .await?;
        let car_branches: BTreeSet<String> = cars
            .iter()
            .filter_map(|c| {
                c.get("metadata")
                    .and_then(|m| m.get("branch"))
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .filter(|b| !b.is_empty())
            .collect();
        // Dedup set: the branches an OPEN backlog-item alarm already
        // names in `metadata.stranded_branch`. A persisting strand is one
        // packet, not one every ten minutes. (A closed-then-still-
        // stranded green re-files — a recurrence after a human answered
        // is a new fact, the same call estate.alarm makes.)
        //
        // Read PAST page one. A bare `limit=200` treated a truncated
        // page as the whole set, so once open backlog-items passed 200
        // the existing alarm sat beyond the page, `already_alarmed`
        // missed it, and the strand re-filed every reconcile pass — a
        // self-compounding flood. `list_all_pages` pages on `total`
        // until every matching row is read (same paginator boarding and
        // the dock preview use), so the dedup set is complete.
        let open_alarms = list_all_pages(|offset| async move {
            self.api(
                Method::GET,
                &format!(
                    "/api/jobs?kind=backlog-item&status=open&limit={PAGE_LIMIT}&offset={offset}"
                ),
                None,
            )
            .await
        })
        .await?;
        let already_alarmed: BTreeSet<String> = open_alarms
            .iter()
            .filter_map(|j| {
                j.get("metadata")
                    .and_then(|m| m.get("stranded_branch"))
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .collect();
        let windows = StrandWindows {
            never_parked_mins: self.cfg.stranded_alarm_mins,
            auto_park_grace_mins: self.cfg.auto_park_grace_mins,
        };
        let to_alarm =
            stranded_greens_to_alarm(&gate_runs, &car_branches, &already_alarmed, now, windows);
        for a in &to_alarm {
            log(format!(
                "reconcile: stranded green {} — gated green {}min, no car, no open alarm; filing backlog-item",
                a.branch, a.age_mins
            ));
            if self.cfg.dry {
                continue;
            }
            self.api(
                Method::POST,
                "/api/jobs",
                Some(stranded_alarm_body(a, windows, now)),
            )
            .await?;
        }
        // A STANDING alarm is refreshed, never twinned: the strand is
        // still true, and the packet should carry today's measurement
        // rather than the age it was filed with (the silence sweep's
        // idiom). `already_alarmed` kept these out of `to_alarm`.
        let stranded_now: BTreeSet<String> =
            crate::census::stranded_gate_runs(&gate_runs, &car_branches)
                .into_iter()
                .collect();
        for j in &open_alarms {
            let Some(branch) = j
                .get("metadata")
                .and_then(|m| m.get("stranded_branch"))
                .and_then(Value::as_str)
            else {
                continue;
            };
            if !stranded_now.contains(branch) {
                continue;
            }
            let (Some(id), Some((gate_run_id, age_mins, park_intent))) = (
                j.get("id").and_then(Value::as_str),
                freshest_green(&gate_runs, branch, now),
            ) else {
                continue;
            };
            if self.cfg.dry {
                continue;
            }
            let measured = StrandedGreen {
                branch: branch.to_string(),
                gate_run_id,
                age_mins,
                cause: StrandCause::from_intent(park_intent),
            };
            self.api(
                Method::PATCH,
                &format!("/api/jobs/{id}/metadata"),
                Some(stranded_refresh_patch(&measured, now)),
            )
            .await?;
        }
        // AND THE ALARM CLOSES ITSELF when the claim stops holding.
        for (id, branch) in stranded_alarms_to_clear(&open_alarms, &stranded_now) {
            let why = stranded_clear_reason(&gate_runs, &car_branches, &branch);
            log(format!(
                "reconcile: stranded-green alarm {} for {branch} no longer holds ({why}); closing it",
                id8(&id)
            ));
            if self.cfg.dry {
                continue;
            }
            let job = self
                .api(Method::GET, &format!("/api/jobs/{id}"), None)
                .await?;
            let Some(job) = job else { continue };
            let Some(step) = find_step(&job, "triage", "Measure the claim, choose a route") else {
                // A packet with no triage step cannot close itself. Say
                // so once rather than silently leaving a false alarm.
                log(format!(
                    "reconcile: stranded-green alarm {} has no triage step; left open",
                    id8(&id)
                ));
                continue;
            };
            let Some(step_id) = step.get("id").and_then(Value::as_str) else {
                continue;
            };
            let existing = metadata_map(step);
            self.api(
                Method::PUT,
                &format!("/api/jobs/{id}/steps/{step_id}"),
                Some(stranded_clear_step_body(&existing, &branch, &why)),
            )
            .await?;
        }
        Ok(())
    }

    /// The dock's SHA-anchored merge preview (12a25f3e): every
    /// parked-ready car gets `metadata.merge_preview` — clean-or-
    /// conflicted vs current main, pairwise conflicts across the
    /// parked set, anchored to main@sha + a parked-set hash so a moved
    /// input reads STALE rather than wrong. Written only on CHANGE
    /// (`dock_preview::changed`): the 10-minute tick is a heartbeat,
    /// not an event source.
    async fn preview_dock(&self, now: DateTime<Utc>) -> Result<()> {
        use crate::dock_preview as dp;
        let clone = &self.cfg.clone;
        // Every parked car needs its merge preview, so read past page
        // one: a tail car left off would silently get no preview, and
        // the pairwise-conflict set would be computed over an incomplete
        // dock — a "clean" preview that hides a real conflict.
        let listed = list_all_pages(|offset| async move {
            self.api(
                Method::GET,
                &format!(
                    "/api/jobs?kind=ship-a-change&status=open&limit={PAGE_LIMIT}&offset={offset}"
                ),
                None,
            )
            .await
        })
        .await?;
        let mut cars: Vec<(String, Value, String)> = Vec::new(); // (id, job, branch)
        for j0 in listed {
            let jid = job_id(&j0)?.to_string();
            if !parked_ready(&j0) {
                continue;
            }
            let Some(branch) = j0
                .pointer("/metadata/branch")
                .and_then(Value::as_str)
                .map(str::to_string)
            else {
                continue;
            };
            cars.push((jid, j0, branch));
        }
        if cars.is_empty() {
            return Ok(());
        }
        // Bring main + every parked branch into temp refs the trial
        // merges can address; refs/preview/* is cleaned each tick so a
        // deleted branch does not linger as a phantom.
        //
        // BEST-EFFORT PER BRANCH (2026-09-06). One car whose branch has
        // vanished — rerailed, deleted, or held with its branch removed —
        // must not abort the whole preview: a single combined fetch with
        // a missing refspec exits rc=128 and blanked the entire dock
        // projection every pass. `main` is required (the baseline); each
        // car branch is fetched on its own, and a car whose ref does not
        // resolve is dropped from the preview (it is not boardable anyway).
        let dir = Some(Path::new(clone.as_str()));
        sh_in(
            dir,
            true,
            &[
                "git",
                "fetch",
                "--quiet",
                "origin",
                "+refs/heads/main:refs/preview/main",
            ],
        )?;
        for (_, _, b) in &cars {
            let refspec = format!("+refs/heads/{b}:refs/preview/{b}");
            // check=false: a vanished branch is expected here and handled
            // by the resolve-and-drop below, not an error.
            let _ = sh_in(dir, false, &["git", "fetch", "--quiet", "origin", &refspec]);
        }
        let rev = |r: &str| -> Option<String> {
            let out = sh_in(dir, false, &["git", "rev-parse", "--verify", "--quiet", r]).ok()?;
            if !out.status.success() {
                return None;
            }
            let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
            (!s.is_empty()).then_some(s)
        };
        let main_sha =
            rev("refs/preview/main").ok_or_else(|| anyhow!("preview: main ref did not resolve"))?;
        let mut pairs: Vec<(String, String)> = Vec::new();
        cars.retain(|(_, _, b)| match rev(&format!("refs/preview/{b}")) {
            Some(sha) => {
                pairs.push((b.clone(), sha));
                true
            }
            None => {
                log(format!(
                    "preview: branch {b} did not resolve (vanished?) — dropped from the dock preview"
                ));
                false
            }
        });
        if cars.is_empty() {
            return Ok(());
        }
        let set = dp::set_hash(clone, &pairs)?;
        let stamp = boss_jobs::car::stamp(now);

        // vs main, then pairwise. n is dock-sized (<=24 by WIP limit);
        // n^2 in-memory merges is cheap next to one real boarding.
        let mut vs_main: Vec<dp::Verdict> = Vec::new();
        for (_, _, b) in &cars {
            vs_main.push(dp::trial_merge(
                clone,
                "refs/preview/main",
                &format!("refs/preview/{b}"),
            )?);
        }
        for (i, (jid, job, b)) in cars.iter().enumerate() {
            let mut co: Vec<(String, Vec<String>)> = Vec::new();
            for (k, (_, _, other)) in cars.iter().enumerate() {
                if i == k {
                    continue;
                }
                if let dp::Verdict::Conflicts(files) = dp::trial_merge(
                    clone,
                    &format!("refs/preview/{b}"),
                    &format!("refs/preview/{other}"),
                )? {
                    co.push((other.clone(), files));
                }
            }
            let fresh = dp::preview_payload(&vs_main[i], &co, &main_sha, &set, &stamp);
            let stored = job.pointer("/metadata/merge_preview");
            if dp::changed(stored, &fresh) && !self.cfg.dry {
                self.merge_job_metadata(jid, vec![("merge_preview", fresh)])
                    .await?;
                log(format!(
                    "{}: merge preview updated (vs-main {}, {} co-boarder conflict(s))",
                    id8(jid),
                    if matches!(vs_main[i], dp::Verdict::Clean) {
                        "clean"
                    } else {
                        "CONFLICT"
                    },
                    co.len(),
                ));
            }
        }
        Ok(())
    }

    /// The stall sentinel: stamp `stalled_since` (once) when an open
    /// train's newest step completion ages past the threshold; clear
    /// the stamp when the train advances. Raising is protocol,
    /// cancelling is judgment — nothing here auto-cancels; the
    /// operator's verb for that is `boss train cancel`.
    async fn note_stall(
        &self,
        t: &Value,
        now: DateTime<Utc>,
        policy: &DeliveryPolicy,
    ) -> Result<()> {
        let tid = job_id(t)?;
        let stamped = truthy(t.get("metadata").and_then(|m| m.get("stalled_since")));
        match stall_age_hours(t, now, policy.stall_hours) {
            Some(age) if !stamped => {
                log(format!(
                    "train {} stalled ({age}h, threshold {}h)",
                    id8(tid),
                    policy.stall_hours
                ));
                // Since WHEN: the newest completion — the moment
                // progress provably stopped, not the moment the
                // sentinel happened to look.
                let since = newest_completion(t).unwrap_or_default().to_string();
                self.merge_job_metadata(tid, vec![("stalled_since", json!(since))])
                    .await?;
            }
            None if stamped => {
                log(format!("train {} advanced — stall stamp cleared", id8(tid)));
                self.merge_job_metadata(tid, vec![("stalled_since", Value::Null)])
                    .await?;
            }
            _ => {}
        }
        Ok(())
    }

    /// Reconcile's arrival sweep: delete landed cars' branches from
    /// the forge (protocol decision, David). The repo auto-deletes
    /// merged `train/*` PR heads, but each CAR branch survives its
    /// squash-merged content landing — and ancestry cannot prove the
    /// landing, so nothing git-side can ever say "safe to sweep".
    /// The job record can: once a train has closed (arrived) and a
    /// boarded car closed with the merged outcome, that branch's
    /// work is on main and the conductor deletes it. A 404 is a fine
    /// answer — something got there first, and the sweep says nothing
    /// about it (see `sweep_note`). A train with nothing left that
    /// could become deletable is stamped `branches_swept`
    /// (`sweep_complete`), so the steady state costs the list read and
    /// no per-car fetches.
    ///
    /// Cost: one jobs-list PAGE per hundred closed trains, plus per
    /// UNSWEPT train one fetch per boarded car, one `branch_head` per
    /// deletable branch, and one delete of the train's own branch (a
    /// silent 404 once it is gone). The `branches_swept` stamp bounds
    /// the per-train work, not the read — the read pages the whole
    /// closed set, because the stamp cannot bound what it has not
    /// seen.
    ///
    /// THE LEAK THIS PAGING FIXES (measured 2026-09-10, packet
    /// 02069932). This read was `limit=50` under a comment claiming
    /// coverage was never capped. It was capped at 50, and the window
    /// turns over fast: a consist check that refuses opens and closes
    /// a cancelled train about once a minute, so ~50 minutes of
    /// refusals push every arrived train off page one. A train is
    /// stamped only once all its cars are terminal, so a car still
    /// open at `proven` — the residue we spend sessions draining —
    /// leaves its train unstamped, and once the window has turned
    /// over that train is never read again and its landed branches
    /// stay on the forge for good. Self-aggravating: proof delay
    /// causes the leak, and proof delay is what we drain.
    ///
    /// Measured: 971 closed trains, eight branches of merged+closed
    /// cars still on the forge, 529 consist-refused trains closed in
    /// the preceding nine hours. Two of those eight belong to train
    /// 82a643b4, whose cars closed 46 seconds AFTER the only reconcile
    /// that pass — it was still inside the window then, so the cap is
    /// not what held those two that hour; every later reconcile exited
    /// on `another conductor run holds the lock` (a separate defect,
    /// backlog), and by the time the sweep runs again the window has
    /// turned over 10 times and the cap is what keeps them leaked.
    async fn sweep_landed_branches(&self) -> Result<()> {
        // Every closed train, not the newest page of them: the one
        // paginator this file shares with candidates,
        // open_car_branches, preview_dock and probe_dock_depth.
        let arrived = list_all_pages(|offset| async move {
            self.api(
                Method::GET,
                &format!(
                    "/api/jobs?kind=pr-train&status=closed&limit={PAGE_LIMIT}&offset={offset}"
                ),
                None,
            )
            .await
        })
        .await?;
        let pending = sweep_pending(&arrived);
        if pending.is_empty() {
            return Ok(());
        }
        // Branches still-open cars name, fetched once per pass: a
        // live car's claim beats any landed car's deletion.
        let open_branches = self.open_car_branches().await?;
        for t in pending {
            // PER-TRAIN ISOLATION (mirrors reconcile's, 2026-09-06).
            // The sweep is housekeeping and its CALLER already keeps the
            // reconcile green — but the sweep had no per-item isolation
            // of its own, so one persistent failure (a boarded car Job
            // deleted → 404 at get_job, a malformed arrival-report
            // PATCH, a forge blip on branch_head/delete_branch) aborted
            // the WHOLE sweep on a `?` every pass. Every LATER pending
            // train then went unswept and its landed `train/*` and car
            // branches accumulated on the forge — the recurring
            // forge-disk fill. Each train now runs in its own fallible
            // scope: a sick train costs itself one pass, not the fleet.
            let t_id = t.get("id").and_then(Value::as_str).unwrap_or("?");
            let outcome: Result<()> = async {
                let tid = job_id(t)?;
                let boarded: Vec<String> = t
                    .get("metadata")
                    .and_then(|m| m.get("boarded_jobs"))
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(|v| v.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default();
                let mut cars = Vec::with_capacity(boarded.len());
                for cid in &boarded {
                    cars.push(self.get_job(cid).await?);
                }
                // The full record, once per unswept train: the arrival
                // report and the branch cleanup both read its steps,
                // which the list rows do not carry.
                let train = self.get_job(tid).await?;
                self.file_arrival_report(&train, &cars).await?;
                self.clean_arrived_train_branch(&train).await;
                // PER-BRANCH ISOLATION. One un-sweepable branch must not
                // strand the train's OTHER landed branches on the forge:
                // a `?` here would abort this train and leave its clean
                // siblings undeleted (disk debt) until the failing one
                // healed. A branch that failed also leaves the train
                // UNSTAMPED below, so it is revisited next pass rather
                // than marked swept with the branch leaked.
                let mut branch_failures = 0usize;
                // THE RECORD OF THIS PASS, one row per branch decided,
                // written on the train beside the stamp so the stamp
                // carries its evidence (1096b1a4: six leaked branches
                // under stamped trains, and the only trace of what the
                // forge had answered was a journal outside the cluster).
                let mut report: Vec<Value> = Vec::new();
                for b in deletable_branches(&cars, &open_branches) {
                    let branch_outcome: Result<()> = async {
                        // The job record proved the CONTENT landed; the head
                        // guard proves the branch still holds only that
                        // content. Both, or the branch stays (car 23923b40).
                        // For a rerail original the recorded head is the one
                        // the rerail read off it, so a commit pushed after the
                        // rerail keeps the original exactly as a commit pushed
                        // after boarding keeps a car's own branch.
                        let current = self.forge.branch_head(&b.branch).await?;
                        let guard = sweep_guard(b.head.as_deref(), current.as_deref());
                        // Verdicts that keep a branch narrate themselves, and
                        // a branch already off the forge narrates nothing.
                        if let Some(note) = sweep_note(&guard, &b) {
                            log(note);
                        }
                        match &guard {
                            SweepGuard::Gone => report.push(sweep_report_row(&b, "gone before this pass")),
                            SweepGuard::NoRecord => report.push(sweep_report_row(&b, "kept: no boarded head on record")),
                            SweepGuard::Moved { recorded, current } => report.push(sweep_report_row(
                                &b,
                                &format!("kept: moved since boarding ({} -> {})", &recorded[..recorded.len().min(8)], &current[..current.len().min(8)]),
                            )),
                            SweepGuard::Delete => {}
                        }
                        if guard == SweepGuard::Delete {
                            let what = sweep_subject(&b);
                            if self.cfg.dry {
                                log(format!(
                                    "DRY: would delete {what} (car {} landed)",
                                    id8(&b.car)
                                ));
                                return Ok(());
                            }
                            // THE DELETE IS OBSERVED, NEVER ASSUMED. The forge's
                            // answer is a claim; the branch read back afterwards
                            // is the fact. A branch still present after an
                            // answered delete is a failure of THIS branch: the
                            // train stays pending, the row says what the forge
                            // said, and the line is loud.
                            let claimed = self.forge.delete_branch(&b.branch).await?;
                            let after = self.forge.branch_head(&b.branch).await?;
                            match sweep_delete_verdict(claimed, after.as_deref()) {
                                SweepDelete::Deleted => {
                                    log(format!("deleted {what} (car {} landed)", id8(&b.car)));
                                    report.push(sweep_report_row(&b, "deleted"));
                                }
                                SweepDelete::AlreadyGone => {
                                    // It existed a moment ago — something else
                                    // swept it between the two calls. Rare, and
                                    // worth saying so it is not read as our doing.
                                    log(format!("{what} already gone (car {} landed)", id8(&b.car)));
                                    report.push(sweep_report_row(&b, "already gone"));
                                }
                                SweepDelete::StillPresent { forge_said } => {
                                    let head = after.unwrap_or_default();
                                    log(sweep_still_present_line(&b, forge_said, &head));
                                    report.push(sweep_report_row(
                                        &b,
                                        &format!("still present after DELETE answered \"{forge_said}\""),
                                    ));
                                    bail!(
                                        "{what} still on the forge after DELETE answered \"{forge_said}\""
                                    );
                                }
                            }
                        }
                        Ok(())
                    }
                    .await;
                    if let Err(e) = branch_outcome {
                        branch_failures += 1;
                        log(sweep_branch_failed_line(&b.branch, &b.car, &e));
                    }
                }
                // A branch withheld for a still-open car's claim is not
                // swept — it is deferred, and it becomes deletable the
                // moment that car closes. Named here so the train's
                // pending state has a stated reason, and counted so the
                // stamp below cannot close over it.
                let deferred = claim_deferred_branches(&cars, &open_branches);
                for b in &deferred {
                    log(claim_deferred_line(b));
                    report.push(sweep_report_row(b, "kept: a still-open car claims it"));
                }
                // Stamp swept only when EVERY branch was handled: a
                // branch we could not sweep this pass must be revisited,
                // and the stamp is what drops the train off the pending
                // list. Stamping over an un-swept branch leaks it onto
                // the forge forever — the very debt this isolation
                // exists to stop. The report is written EITHER WAY, in
                // the same PUT as the stamp when there is one: an
                // unstamped train says on its own record why it is
                // still pending, and a stamped one says what each
                // branch's delete was observed to do.
                let mut kv: Vec<(&str, Value)> = vec![("sweep_report", json!(report))];
                if sweep_complete(branch_failures, deferred.len(), &cars) {
                    kv.push(("branches_swept", json!("true")));
                }
                self.merge_job_metadata(tid, kv).await?;
                Ok(())
            }
            .await;
            if let Err(e) = outcome {
                log(sweep_train_failed_line(t_id, &e));
            }
        }
        Ok(())
    }

    /// File the arrival report — the landing's final structured entry
    /// — on an arrived train's `arrived` step, once. The sweep is the
    /// conductor's visit to every arrived train, so the report is
    /// composed here from the full job record plus the boarded cars
    /// the sweep already fetched. The step PUT merges metadata (the
    /// same rule `overlay_metadata` pins): the outcome step's own
    /// keys survive the filing.
    async fn file_arrival_report(&self, train: &Value, cars: &[Value]) -> Result<()> {
        let tid = job_id(train)?;
        let Some(step) = find_step(train, "arrived", "Train arrived") else {
            return Ok(());
        };
        let filed = arrival_already_filed(train);
        // Strictly `completed` — never `skipped`: a cancelled train
        // closes with its arrived step SKIPPED, and a landing report
        // on a train that never landed would be fiction.
        let arrived = step.get("status").and_then(Value::as_str) == Some("completed");
        if !arrived || filed {
            return Ok(());
        }
        let report = arrival_report(train, cars);
        let summary = arrival_summary(&report);
        let n = report
            .get("consist")
            .and_then(Value::as_array)
            .map_or(0, Vec::len);
        let total = report
            .get("timings")
            .and_then(|t| t.get("total_s"))
            .and_then(Value::as_i64)
            .map_or_else(|| "?".to_string(), |s| s.to_string());
        if self.cfg.dry {
            log(format!(
                "DRY: would file the arrival report on {}",
                id8(tid)
            ));
            return Ok(());
        }
        // THE REPORT LANDS ON THE JOB, NOT THE STEP (defect f402a681).
        //
        // It used to PUT onto the `arrived` step's metadata — and the
        // guard above requires that step to be `completed`, so the only
        // write this function could ever attempt was a write to a
        // TERMINAL step. Once terminal steps became immutable, every
        // attempt returned 409 "step is terminal — these fields are
        // immutable", and because this returns Err, it took
        // `sweep_landed_branches` down with it on the `?` at the call
        // site: no arrival report AND no branch cleanup, every ten
        // minutes, for weeks.
        //
        // The 409's own hint names the fix: "To correct or annotate it,
        // write to the parent job's metadata (PATCH /api/jobs/{id}/
        // metadata) instead." That endpoint MERGES top-level keys, so
        // no overlay is needed here — the merge is the server's job.
        //
        // The report is a fact ABOUT the train, not a field of the
        // transition that recorded arrival, so the job is where it
        // belonged anyway. `summary` is written as `arrival_summary`
        // because a bare `summary` on job metadata is a name anything
        // could want.
        self.api(
            Method::PATCH,
            &format!("/api/jobs/{tid}/metadata"),
            Some(json!({"arrival_report": report, "arrival_summary": summary})),
        )
        .await?;
        log(format!(
            "arrival report on {} ({n} cars, total {total}s)",
            id8(tid)
        ));
        Ok(())
    }

    /// Housekeeping at arrival: the train's OWN branch comes off the
    /// forge once the landing is on the record — the same forge
    /// delete the cancel path has always used, now owned by the happy
    /// path too (`arrival_branch_to_delete` says when and which).
    /// Infallible by signature: a delete that fails is a journal line
    /// and the arrival stands — a leftover branch is debt, a failed
    /// arrival is an outage.
    async fn clean_arrived_train_branch(&self, train: &Value) {
        let Some(branch) = arrival_branch_to_delete(train, &self.cfg.forge_kind) else {
            return;
        };
        if self.cfg.dry {
            log(format!("DRY: would delete branch {branch} (train arrived)"));
            return;
        }
        let outcome = self.forge.delete_branch(&branch).await;
        if let Some(note) = arrival_cleanup_note(&branch, outcome) {
            log(note);
        }
    }

    /// The branches named by still-open ship-a-change cars — never
    /// deletable, whoever landed on them. Read off the list rows
    /// (the jobs list returns full metadata); an open car with no
    /// branch yet contributes nothing.
    async fn open_car_branches(&self) -> Result<BTreeSet<String>> {
        // Every open car's branch, past page one: an older open car
        // sorts to the tail, and a capped read that misses it would let
        // the sweep delete a branch a still-open car names.
        let listed = list_all_pages(|offset| async move {
            self.api(
                Method::GET,
                &format!(
                    "/api/jobs?kind=ship-a-change&status=open&limit={PAGE_LIMIT}&offset={offset}"
                ),
                None,
            )
            .await
        })
        .await?;
        Ok(listed
            .iter()
            .filter_map(|j| {
                j.get("metadata")
                    .and_then(|m| m.get("branch"))
                    .and_then(Value::as_str)
                    .filter(|b| !b.is_empty())
                    .map(str::to_string)
            })
            .collect())
    }

    // -----------------------------------------------------------------------
    // Phase 2 — board this window's train
    // -----------------------------------------------------------------------

    fn ensure_clone(&self) -> Result<()> {
        let clone = &self.cfg.clone;
        if !Path::new(clone).join(".git").is_dir() {
            // A dir left from a partial/interrupted clone — present but
            // with no .git — makes `git clone` refuse ("destination
            // exists and is not empty"). Swallowed by a caller's .ok(),
            // that leaves reconcile with no clone and every superseded
            // train wedged at `converged` (2026-09-04: three trains, and
            // the reconcile ran in 0s because the clone fast-failed).
            // Clear the stale dir so the clone can proceed; a valid clone
            // has .git and never reaches here.
            if Path::new(clone).exists() {
                let _ = fs::remove_dir_all(clone);
            }
            fs::create_dir_all(&self.cfg.home)?;
            sh(&["git", "clone", &self.cfg.upstream_url, clone])?;
            sh(&[
                "git",
                "-C",
                clone,
                "remote",
                "add",
                "fork",
                &self.cfg.fork_url,
            ])?;
            // The merge commits the assembly makes need an author, and the
            // honest one is the machine that made them (a fresh clone has
            // no identity — the first real run failed exactly here).
            sh(&[
                "git",
                "-C",
                clone,
                "config",
                "user.name",
                "BOSS train conductor",
            ])?;
            sh(&[
                "git",
                "-C",
                clone,
                "config",
                "user.email",
                "train-conductor@boss.invalid",
            ])?;
        }
        sh(&["git", "-C", clone, "fetch", "origin", "--prune"])?;
        sh(&["git", "-C", clone, "fetch", "fork", "--prune"])?;
        Ok(())
    }

    /// The parked-ready cars whose branch is actually on the fork,
    /// plus the left-behind record for the ones whose branch is not
    /// — each of those gets its `skip_reason` stamped (the yard's
    /// "LEFT BEHIND" chip) and an entry for the train's own books.
    async fn candidates(&self) -> Result<(Vec<(Value, String)>, Vec<Value>)> {
        let mut out = Vec::new();
        let mut left_behind = Vec::new();
        // EVERY open car, not just page one. A car opened days ago but
        // parked today sorts to the tail (`ORDER BY opened_on DESC`), so
        // a bare `limit=` boards nothing from the tail once the backlog
        // passes a page — the silent starvation this fix exists for.
        let listed = list_all_pages(|offset| async move {
            self.api(
                Method::GET,
                &format!(
                    "/api/jobs?kind=ship-a-change&status=open&limit={PAGE_LIMIT}&offset={offset}"
                ),
                None,
            )
            .await
        })
        .await?;
        for j0 in listed {
            let jid = job_id(&j0)?.to_string();
            let j = self.get_job(&jid).await?;
            if !parked_ready(&j) {
                continue;
            }
            // The two-strike hold. Without it the auto-cancel above is
            // a loop: the same consist re-boards, goes red, cancels,
            // and burns the night landing nothing.
            if let Some(reason) = car_hold_reason(&j, self.policy.max_red_trains) {
                log(format!("{}: {reason} — leaving behind", id8(&jid)));
                left_behind.push(json!({"car_id_short": id8(&jid), "reason": reason.as_str()}));
                if !self.cfg.dry {
                    self.merge_job_metadata(&jid, vec![("skip_reason", json!(reason))])
                        .await?;
                }
                continue;
            }
            // THE DECLARED ORDERING EDGE (d3320278). Judged here, beside
            // the two-strike hold, because both answer "this car is green
            // and still must not ride yet" — a question about the car's
            // WORLD, not its content — and both are cheaper than the git
            // work below. A car with no edge costs nothing: no read is
            // made at all, which is what keeps the regression surface of
            // this change to the cars that opt in.
            if let Some(hold) = self.edge_hold(&j, &jid).await {
                log(format!("{}: {} — leaving behind", id8(&jid), hold.reason));
                left_behind.push(json!({
                    "car_id_short": id8(&jid),
                    "reason": hold.reason.as_str(),
                    EDGE_HOLD: hold.kind,
                }));
                if !self.cfg.dry {
                    self.merge_job_metadata(&jid, vec![("skip_reason", json!(hold.reason))])
                        .await?;
                }
                continue;
            }
            let branch = j
                .get("metadata")
                .and_then(|m| m.get("branch"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let ok = sh_unchecked(&[
                "git",
                "-C",
                &self.cfg.clone,
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("fork/{branch}"),
            ])?;
            // RECOVER RATHER THAN SKIP. A car is parked at review by its
            // author pushing the branch; the natural place to push is
            // the upstream the author cloned, and the fork is an
            // implementation detail of how this conductor assembles a
            // train. On 2026-08-14 that gap silently held NINE cars for
            // a whole session: the dock reported 12 parked while the
            // boardable count was 0, because `parked_ready` asks
            // "branch declared, review ready" and this asks "branch on
            // the fork" — two predicates for one question, and only the
            // first is on any dashboard.
            //
            // So if the branch exists upstream, put it on the fork and
            // board the car. Copying a ref the author already published
            // is not a judgement call; refusing to, and reporting a
            // dock depth that cannot board, is the surprising
            // behaviour. A branch that exists in NEITHER place is still
            // a real skip — that car was never pushed at all.
            // ABSENT **OR STALE**. Existence is not the question: a
            // branch already on the forge is never refreshed, so a car
            // fixed after a red train boards the commit that failed.
            let fork_sha = if ok.status.success() {
                Some(String::from_utf8_lossy(&ok.stdout).trim().to_string())
            } else {
                None
            };
            let want = car_head(&self.cfg.clone, &branch)?;
            let stale = matches!((&fork_sha, &want), (Some(f), Some(w)) if f != w);
            let mut ok = ok;
            if (!ok.status.success() || stale)
                && !self.cfg.dry
                && publish_car_branch(&self.cfg.clone, &branch)?
            {
                log(format!(
                    "{}: branch {branch} was not on the fork — published it",
                    id8(&jid)
                ));
                ok = sh_unchecked(&[
                    "git",
                    "-C",
                    &self.cfg.clone,
                    "rev-parse",
                    "--verify",
                    "--quiet",
                    &format!("fork/{branch}"),
                ])?;
            }
            if !ok.status.success() {
                let reason = skip_reason_branch_missing(&branch);
                log(format!("{}: {reason} — leaving behind", id8(&jid)));
                left_behind.push(json!({"car_id_short": id8(&jid), "reason": reason.as_str()}));
                if !self.cfg.dry {
                    // Loud on the Job, not just in the journal: the author
                    // parked this at review believing it would board.
                    self.merge_job_metadata(&jid, vec![("skip_reason", json!(reason))])
                        .await?;
                }
                continue;
            }
            // The receipt spot-check (742d1faa): the head this car will
            // actually board must be the head its gate receipt vouches
            // for, and the receipt must be green on a clean tree.
            //
            // THE HEAD THAT BOARDS IS THE ONE ON THE FORK. The consist is
            // assembled from `fork/{branch}` (`rerail_onto_consist`) and
            // the boarded head is stamped from it, so that ref — not the
            // conductor clone's own `refs/heads` — is what the receipt
            // has to match. `car_head` prefers the LOCAL branch, which is
            // the right question for "is there anything newer to publish"
            // and the wrong answer to "what will ride". The two come
            // apart when a car is rebased and re-pushed, which is the
            // normal repair: the clone keeps the pre-rebase commit, the
            // push cannot fast-forward past it, the fork rightly keeps
            // the gated commit, and comparing the receipt against the
            // local head leaves a correctly-gated car behind for "gated,
            // then changed". Read on 2026-08-29 from a live dock — car
            // c6531868 was held out with its receipt (56b817eb) matching
            // the fork exactly, against a local ref eight hours older.
            //
            // Read AFTER the publish attempt above, so a branch that was
            // just published is judged on what actually landed there
            // rather than on what was offered.
            let boards = fork_head(&self.cfg.clone, &branch)?;
            if let Some(reason) = receipt_skip_reason(&j, boards.as_deref()) {
                log(format!("{}: {reason} — leaving behind", id8(&jid)));
                left_behind.push(json!({"car_id_short": id8(&jid), "reason": reason.as_str()}));
                if !self.cfg.dry {
                    self.merge_job_metadata(&jid, vec![("skip_reason", json!(reason))])
                        .await?;
                }
                continue;
            }
            out.push((j, branch));
        }
        Ok((out, left_behind))
    }

    /// Does this car's DECLARED ORDERING EDGE hold it back?
    /// `None` = board it (no edge, a satisfied edge, or an edge this pass
    /// could not judge).
    ///
    /// INFALLIBLE BY SIGNATURE, deliberately, and that is the whole of
    /// the safety argument. This runs inside the loop that boards every
    /// train; a `?` here would let one unreadable packet refuse the
    /// entire window, and the gate never runs the conductor, so nothing
    /// before production would have caught it (CLAUDE.md: a fallible
    /// write in reconcile froze ALL landings). So every failure becomes
    /// `Predecessor::Unreadable`, which boards the car and journals WHY
    /// — loud, per CLAUDE.md §Diagnosis, because a swallowed read is the
    /// next diagnosis paid for in advance.
    ///
    /// The 404 is separated from the blips on purpose: "there is no such
    /// Job" is an ANSWER and a hold a person must fix, while "I could not
    /// ask" is neither. `api` has already exhausted its retry budget by
    /// the time either reaches here.
    async fn edge_hold(&self, car: &Value, jid: &str) -> Option<EdgeHold> {
        let declared = declared_predecessor(car)?;
        let pred = match self
            .api(Method::GET, &format!("/api/jobs/{declared}"), None)
            .await
        {
            Ok(Some(p)) => Predecessor::Found(p),
            // A success with no body is the same fact as a 404 for this
            // question: the system of record served nothing for that id.
            Ok(None) => Predecessor::Absent,
            Err(e) if is_no_such_job(&e) => Predecessor::Absent,
            Err(e) => Predecessor::Unreadable(short_cause(&e, self.policy.blip_cause_budget)),
        };
        match boards_after_outcome(&declared, &pred) {
            EdgeOutcome::Board => None,
            EdgeOutcome::BoardUnjudged(note) => {
                log(format!("{}: {note}", id8(jid)));
                None
            }
            EdgeOutcome::Hold(h) => Some(h),
        }
    }

    async fn open_train_job(&self, train_branch: &str, window: &str) -> Result<Option<Value>> {
        // THE PIN. The train records the policy version it is departing
        // under, so an edit made while it is in flight cannot rewrite
        // the rules it left on — the same promise a packet gets from the
        // workflow version it was admitted under. Nothing is stamped
        // when the conductor fell back to compiled values: there is no
        // version, and a record that claimed one would be lying.
        let mut metadata = Map::new();
        metadata.insert("actor".to_string(), json!(ACTOR));
        for (k, v) in delivery_policy::pin_stamps(&self.policy) {
            metadata.insert(k.to_string(), v);
        }
        let payload = json!({
            "kind": "pr-train",
            "subject": {"subject_kind": "custom", "id": train_branch},
            "title": format!("PR train {window}"),
            // The conductor is a machine and says so. `resolve_owner`
            // reads any colon-bearing id as automation and places the
            // Job on an active holder of the kind's `owner_role`
            // (`platform-admin` for pr-train) — so the responsible
            // human is whoever actually holds the role today.
            //
            // This used to name `emp-bootstrap-admin` outright, which
            // survived only because that row happened to be the
            // deployment's admin. Once the bootstrap identity is
            // retired in favour of a named person, a hardcoded owner
            // is a dead id that resolution has to quietly override —
            // right by accident rather than by construction.
            "owner_id": ACTOR,
            "status": "open",
            "priority": "standard",
            "metadata": metadata,
            "tags": ["train"],
        });
        if self.cfg.dry {
            log(format!("DRY: would open train Job for {train_branch}"));
            return Ok(None);
        }
        let created = self.api(Method::POST, "/api/jobs", Some(payload)).await?;
        let jid = created
            .as_ref()
            .and_then(|c| c.get("id"))
            .and_then(Value::as_str)
            .map(str::to_string);
        let jid = match jid {
            Some(id) => id,
            None => {
                // some create paths return the row wrapped
                let listed = rows(
                    self.api(
                        Method::GET,
                        "/api/jobs?kind=pr-train&status=open&limit=5",
                        None,
                    )
                    .await?,
                )?;
                job_id(
                    listed
                        .first()
                        .ok_or_else(|| anyhow!("no open pr-train Job found after create"))?,
                )?
                .to_string()
            }
        };
        Ok(Some(self.get_job(&jid).await?))
    }

    /// The CI host's boarding verdict, from the estate's host-scope
    /// observation series. `BOSS_TRAIN_CI_HOST` names the estate node
    /// id of the box CI runs on; a deployment that has not configured
    /// it gets exactly the old behaviour, minus silence — one journal
    /// line says the check did not run.
    async fn ci_host_readiness(&self, now: DateTime<Utc>) -> host_readiness::Readiness {
        use crate::host_readiness::Readiness;
        let Some(host) = self.cfg.ci_host.as_deref() else {
            log("ci host check skipped — BOSS_TRAIN_CI_HOST unset");
            return Readiness::Proceed;
        };
        // `scope=host` so the page is not spent by the faster cluster
        // series (the reader's own lesson, 2026-09-02); `limit=50` is
        // its hard cap, depth enough to find this host among the other
        // host-scope observers.
        let fetched = self
            .api(
                Method::GET,
                "/api/estate/observations?scope=host&limit=50",
                None,
            )
            .await;
        match fetched {
            Ok(Some(body)) => host_readiness::host_readiness(
                &body,
                host,
                self.policy.ci_host_floor_gb,
                host_readiness::max_observation_age(),
                now,
            ),
            Ok(None) => Readiness::Unverifiable {
                reason: "the observations reader answered nothing".to_string(),
            },
            Err(e) => Readiness::Unverifiable {
                reason: format!("the observations reader is unreachable ({e})"),
            },
        }
    }

    async fn board(&self, now: DateTime<Utc>) -> Result<()> {
        // Minute precision, not an AM/PM half-day. Boardings fire on
        // dock depth (min 4, 120m cooldown), not a twice-daily clock, so
        // the old "{date} AM/PM" label both COLLIDED — two trains carried
        // an identical "PM" the night of 2026-08-31 — and implied a
        // schedule the system does not run (21d4f433). Mirrors the
        // train_branch stamp on the next line.
        let window = now.format("%Y-%m-%d %H:%M").to_string();
        let train_branch = format!("train/{}", now.format("%Y%m%d-%H%M"));

        // THE TRACK — one train MERGING at a time. The cadence loop
        // holds a departure before it claims a window (cadence::decide),
        // so this is the backstop for a hand-run `boss train board` and
        // for two conductors racing: an open PRE-MERGE pr-train packet
        // means the previous consist has not landed on main, and a
        // second consist assembled now would merge onto a main the
        // first is about to change (a8c6773b). A MERGED train waiting
        // to deploy or converge does not hold it (`holds_the_track`):
        // the next consist merges on top of its content and converges
        // it by ancestry — holding for it deadlocked delivery twice on
        // 2026-09-07 (f3796323). No packet is opened for a hold — it is
        // not a refusal, the yard is not empty, and the next tick after
        // the track clears departs.
        //
        // Every page: the list rows carry `steps` (http/jobs.rs enriches
        // each row), which is what the predicate reads, and the one
        // pre-merge train that matters may sit behind merged ones
        // waiting to converge — a limit is not a filter.
        let on_track = list_all_pages(|offset| async move {
            self.api(
                Method::GET,
                &format!("/api/jobs?kind=pr-train&status=open&limit={PAGE_LIMIT}&offset={offset}"),
                None,
            )
            .await
        })
        .await?;
        if let Some(occupant) = track_occupied_by(&on_track) {
            log(format!("BOARDING HELD — track occupied by {occupant}"));
            return Ok(());
        }

        // THE HOST CHECK — before anything is assembled. On 2026-09-03
        // the conductor boarded two consists onto a CI host whose disk
        // was full, and each burned a full CI cycle discovering it; the
        // locomotive's run-start floor had even PASSED at 01:26,
        // because a start-of-run check cannot see a consist's
        // mid-flight consumption. So the question is asked here, from
        // the estate's observed series, before the first merge is
        // attempted (David, 2026-09-03: "protocol should actually
        // verify before anyone bothers to even start").
        //
        // Only a POSITIVE "the host is short" refuses. Unverifiable —
        // an absent, stale, or unreadable series — proceeds with one
        // loud line, deliberately FAIL-OPEN: the host-scope observer
        // (infra/estate/observe-host.sh) is not yet installed anywhere,
        // and landing this check must not stop all boarding on the day
        // the series does not exist yet. Once the series is live,
        // tightening stale-to-refuse is a policy question, not a
        // rebuild.
        match self.ci_host_readiness(now).await {
            host_readiness::Readiness::Refuse { reason } => {
                // The refusal is the journal's, not a packet's (see "A
                // BOARD THAT DEPARTS NO TRAIN OPENS NO PACKET"). The
                // condition itself — a host short of disk — is already
                // a packet: the estate observer files and refreshes one
                // for the host, and it does not arrive once a minute.
                log(no_departure_line(&NoDeparture::HostShort { reason }));
                return Ok(());
            }
            host_readiness::Readiness::Unverifiable { reason } => {
                log(format!(
                    "ci host unverifiable — {reason} — boarding anyway (fail-open until \
                     the host observation series exists)"
                ));
            }
            host_readiness::Readiness::Proceed => {}
        }

        self.ensure_clone()?;
        let (cands, mut left_behind) = self.candidates().await?;
        if self.cfg.dry {
            log(format!("DRY: candidates: {}", py_pairs(&cands)));
            log(format!(
                "DRY: would assemble {train_branch} and, if the consist check passes, open its \
                 train Job for {window}"
            ));
            return Ok(());
        }

        if cands.is_empty() {
            // NOT NECESSARILY AN IDLE WINDOW. `NothingParked` says "an
            // idle window, not a failure", which is a lie when the dock is
            // full of cars the ordering filter held — and whether this
            // window is self-clearing or waiting on a person is precisely
            // what an operator reads the line to learn.
            log(no_departure_line(&empty_dock_refusal(&left_behind)));
            return Ok(());
        }

        let clone = &self.cfg.clone;
        sh(&[
            "git",
            "-C",
            clone,
            "checkout",
            "-B",
            &train_branch,
            "origin/main",
        ])?;
        // (car, branch, boarded head) — the head is WHAT boarded, and
        // the sweep's licence to delete the branch later depends on it
        // (car 23923b40). Read from the fetched `fork/<branch>` ref,
        // which is precisely the commit the merge below carries.
        let mut boarded: Vec<(Value, String, String)> = Vec::new();
        let mut skipped: Vec<(Value, String)> = Vec::new();
        for (j, branch) in cands {
            let head_out = sh(&["git", "-C", clone, "rev-parse", &format!("fork/{branch}")])?;
            let head = stdout_str(&head_out).trim().to_string();
            let r = sh_unchecked(&[
                "git",
                "-C",
                clone,
                "merge",
                "--no-ff",
                "-m",
                &format!("train: merge {branch}"),
                &format!("fork/{branch}"),
            ])?;
            if r.status.success() {
                boarded.push((j, branch, head));
            } else {
                let diff =
                    sh_unchecked(&["git", "-C", clone, "diff", "--name-only", "--diff-filter=U"])?;
                let conflicted: Vec<String> = stdout_str(&diff)
                    .split_whitespace()
                    .map(str::to_string)
                    .collect();
                sh_unchecked(&["git", "-C", clone, "merge", "--abort"])?;

                // Before abandoning it, try re-railing.
                //
                // The commonest conflict here is not a real one. The
                // repo squash-merges, so a car cut before the last
                // train — or stacked on a car that has since landed —
                // carries commits whose CHANGES are already in main but
                // whose SHAS are not ancestors of it. Merging re-applies
                // landed hunks on top of themselves and collides.
                //
                // `git rebase` is the tool that knows the difference: it
                // drops a patch already present upstream. So replay the
                // car's own commits onto the consist as it stands and
                // merge that instead. A car with a GENUINE conflict
                // fails the rebase too and is skipped exactly as before.
                //
                // Measured cost of not doing this: four cars re-railed
                // by hand in one evening (2026-08-15), each one a fresh
                // branch name, a repointed `metadata.branch` and a wait
                // for the next window — and the same by hand on 08-12
                // and 08-14. The conductor already knows everything it
                // needs; it just gave up one step early.
                if let Some(rerailed) = rerail_onto_consist(clone, &train_branch, &branch)? {
                    let retry = sh_unchecked(&[
                        "git",
                        "-C",
                        clone,
                        "merge",
                        "--no-ff",
                        "-m",
                        &format!("train: merge {branch} (re-railed)"),
                        &rerailed,
                    ])?;
                    if retry.status.success() {
                        log(format!(
                            "{branch}: re-railed onto the consist — its base was no longer an \
                             ancestor of main"
                        ));
                        // The ORIGINAL head is still what boarded: the
                        // sweep's licence to delete the branch compares
                        // against the ref the car names, and re-railing
                        // changed the shas we merged, not the car.
                        boarded.push((j, branch, head));
                        continue;
                    }
                    sh_unchecked(&["git", "-C", clone, "merge", "--abort"])?;
                }
                // ONE reason string, journal and Job alike — the chip
                // the yard renders and the line the operator greps
                // must never tell different stories.
                let reason = skip_reason_conflict(&conflicted, self.policy.skip_reason_file_budget);
                log(format!("{branch}: {reason} — left for the next train"));
                left_behind.push(json!({
                    "car_id_short": id8(job_id(&j)?),
                    "reason": reason.as_str(),
                }));
                self.merge_job_metadata(job_id(&j)?, vec![("skip_reason", json!(reason))])
                    .await?;
                skipped.push((j, branch));
            }
        }

        let skipped_names = skipped
            .iter()
            .map(|(_, b)| b.as_str())
            .collect::<Vec<_>>()
            .join(", ");

        if boarded.is_empty() {
            log(no_departure_line(&NoDeparture::AllConflicted {
                branches: skipped_names.clone(),
            }));
            return Ok(());
        }

        // THE CONSIST CHECK — the assembled tree answers the cheap
        // questions before the train spends anything on the expensive
        // ones. See the section comment above `consist_check` for the
        // arrival-rate numbers that bought it; the short version is
        // that a per-branch gate cannot see a failure that exists only
        // in the combination, and every failure of the last two days
        // was one of those.
        //
        // Placed BEFORE the push, not merely before the PR: a refused
        // consist should leave nothing behind on the forge to clean up
        // later (the 62 stale `train/*` branches of ab3fa473 are what
        // that debt looks like when nobody owns it).
        //
        // Freshen the trunk ref FIRST. The cheap lints resolve their
        // baseline as `merge-base(origin/main, HEAD)` in this clone,
        // and a train that landed since this board's `ensure_clone`
        // leaves that ref lagging behind the assembled tree — which
        // reads already-landed changes as this consist's own and
        // refuses it (2026-09-06). Best-effort: a failed fetch logs
        // and the lints use the ref as it stands, exactly as before.
        freshen_trunk(clone);
        let verdict = consist_check(Path::new(clone), &self.policy);
        for w in verdict.warnings() {
            log(format!(
                "consist check: {w} — skipping it, a broken check must not hold a train"
            ));
        }
        if let ConsistVerdict::Refuse { failed, ran, .. } = &verdict {
            let reason = consist_refusal_reason(failed, self.policy.skip_reason_file_budget);
            log(format!(
                "consist check: {} of {ran} checks disagree with the assembled tree",
                failed.len()
            ));
            // The output goes in the journal in full, not just the
            // name: what cost 90 minutes was learning ONE bit per
            // attempt, and the bit is in what the check SAID.
            for f in failed {
                log(format!("consist check: {} said —", f.name));
                for line in f.output.lines() {
                    log(format!("consist check:   {line}"));
                }
            }
            // NOBODY'S CAR IS AT FAULT. Each one was green on its own
            // branch; the tree only broke once they were merged
            // together. So: no train packet, no PR, no push, no CI spent
            // — and every car keeps `metadata.train` unset (never
            // boarded, so still `parked_ready`) and `red_trains`
            // untouched. Striking cars for a combination failure is the
            // bug we already know about.
            //
            // THE EVIDENCE RIDES THE CARS, not a cancelled train. It
            // used to live in `consist_check` on a pr-train Job that
            // existed only to be cancelled (4860aff8); the car whose
            // boarding it blocks is both the honest owner of the fact
            // and where an operator is already looking. `skip_reason`
            // names the check and the files; `consist_refusal` carries
            // what each check SAID, in full, because what cost 90
            // minutes on 2026-09-04 was learning one bit per attempt.
            // Both are cleared in the same write that stamps a later
            // boarding, so neither outlives the refusal.
            let refusal = json!({
                "verdict": "refused",
                "checks_run": ran,
                "failed": failed
                    .iter()
                    .map(|f| json!({
                        "lint": f.name,
                        "files": f.files,
                        "output": f.output,
                    }))
                    .collect::<Vec<_>>(),
            });
            for (j, _branch, _head) in &boarded {
                let cid = job_id(j)?;
                self.merge_job_metadata(
                    cid,
                    vec![
                        ("skip_reason", json!(reason)),
                        ("consist_refusal", refusal.clone()),
                    ],
                )
                .await?;
            }
            log(no_departure_line(&NoDeparture::ConsistRefused {
                reason: format!("consist check refused — {reason}"),
                cars: boarded.len(),
            }));
            return Ok(());
        }
        log(format!(
            "consist check: {} cheap lint(s) clean on the assembled tree",
            verdict.ran()
        ));

        sh(&["git", "-C", clone, "push", "fork", &train_branch])?;
        let train_ref_out = sh(&["git", "-C", clone, "rev-parse", "--short", "HEAD"])?;
        let train_ref = stdout_str(&train_ref_out).trim().to_string();

        // THE PACKET OPENS HERE — one call site, after the consist check
        // passed and after the branch is on the forge, so a pr-train Job
        // exists only for a train that is actually departing (4860aff8).
        // AFTER the push on purpose: a push that fails leaves one stale
        // `train/*` branch, while a packet opened for a train that never
        // pushed HOLDS THE TRACK until a human cancels it.
        let Some(train) = self.open_train_job(&train_branch, &window).await? else {
            // `None` is the dry-run answer and a dry run returned long
            // before the clone was touched. Say it, rather than running
            // on with no packet to record anything against.
            log("no train departed — the train Job was not opened; nothing further attempted");
            return Ok(());
        };
        let train_id = job_id(&train)?.to_string();

        let mut lines: Vec<String> = boarded
            .iter()
            .map(|(j, b, _)| {
                format!(
                    "- `{b}` — {} (Job `{}`)",
                    j.get("title").and_then(Value::as_str).unwrap_or_default(),
                    id8(j.get("id").and_then(Value::as_str).unwrap_or("?"))
                )
            })
            .collect();
        if !skipped.is_empty() {
            lines.push(String::new());
            lines.push(format!(
                "Left behind on merge conflicts (next train): {skipped_names}"
            ));
        }
        let body = format!(
            "The {window} train: {} change(s) batched by the conductor.\n\n{}\n\n\
             🤖 opened by `boss train` (pr-train Workflow)",
            boarded.len(),
            lines.join("\n")
        );
        let pr_url = self
            .forge
            .pr_create(
                &self.cfg.gh_repo,
                &train_branch,
                &format!("train: {window} ({} changes)", boarded.len()),
                &body,
            )
            .await?;

        let boarded_ids: Vec<String> = boarded
            .iter()
            .map(|(j, _, _)| job_id(j).map(str::to_string))
            .collect::<Result<_>>()?;
        let skipped_branches: Vec<String> = skipped.iter().map(|(_, b)| b.clone()).collect();
        self.merge_job_metadata(
            &train_id,
            vec![
                ("boarded_jobs", json!(boarded_ids)),
                ("skipped_branches", json!(skipped_branches)),
                // The train's own record of who it left behind and
                // why — the arrival report reads THIS, because a
                // car's skip_reason clears the moment a later train
                // boards it.
                ("left_behind", json!(left_behind)),
            ],
        )
        .await?;
        let train = self.get_job(&train_id).await?;
        let boarded_note = boarded
            .iter()
            .map(|(j, b, _)| {
                format!(
                    "{b} ({})",
                    id8(j.get("id").and_then(Value::as_str).unwrap_or("?"))
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        // THE SHA EACH CAR CONTRIBUTED, recorded because twice on
        // 2026-08-17 a consist carried a commit nobody intended and
        // nothing said so: `feat/dev-shared-target` was 3370b42
        // locally and 96109f7 on the forge, and the train assembled
        // the stale one silently. The head is already resolved to
        // board the car, so writing it down costs nothing and turns
        // "which commit did this train actually carry" from a hand
        // diff into a field.
        let heads_note = boarded
            .iter()
            .map(|(j, b, _)| {
                // `fork/<branch>` on purpose, not the local ref: this
                // records what the train ASSEMBLED FROM, which is the
                // thing a reader needs when a consist misbehaves.
                let sha = sh_unchecked(&[
                    "git",
                    "-C",
                    &self.cfg.clone,
                    "rev-parse",
                    "--short",
                    "--verify",
                    "--quiet",
                    &format!("fork/{b}"),
                ])
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "unknown".to_string());
                format!(
                    "{} {b}@{sha}",
                    id8(j.get("id").and_then(Value::as_str).unwrap_or("?"))
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        self.complete_step(
            &train,
            find_step(&train, "collect", "Collect what is ready to board"),
            &[("boarded", Some(boarded_note))],
        )
        .await?;
        self.complete_step(
            &train,
            find_step(&train, "assemble", "Assemble the train branch"),
            &[
                ("train_ref", Some(format!("{train_branch}@{train_ref}"))),
                ("car_heads", (!heads_note.is_empty()).then_some(heads_note)),
                (
                    "skipped",
                    Some(if skipped_names.is_empty() {
                        "none".to_string()
                    } else {
                        skipped_names.clone()
                    }),
                ),
            ],
        )
        .await?;
        self.complete_step(
            &train,
            find_step(&train, "pr", "Open the batched PR"),
            &[("pr_url", Some(pr_url.clone()))],
        )
        .await?;

        for (j, _branch, head) in &boarded {
            // BOARDING DOES NOT COMPLETE `review` — the merge does.
            //
            // It used to complete it here, and that quietly made
            // cancelling a loaded train impossible. A released car has
            // to become `parked_ready` again, which requires its review
            // step to be ready or active; but a completed step is FROZEN
            // at the row (`update_step_at` pins status, completed_on and
            // metadata on terminal rows, deliberately, so a racing
            // read-modify-write cannot demote it). So the cancel path's
            // reopen was a no-op that returned 204, and every "released
            // car back to the dock" line it logged was false — the car
            // had `train` cleared but stayed unboardable forever. The
            // only reason nobody hit it is that every cancel until now
            // carried zero cars.
            //
            // Boarded-ness does not need the step at all: it is
            // `metadata.train`, which is what `parked_ready` already
            // reads, and which a cancel can clear because metadata is
            // not frozen. So the step keeps meaning what it says —
            // this change is open for review until it lands — and
            // release becomes a metadata write with nothing to reverse.
            // (Requires no workflow edit: the spec still gates the
            // `merged` outcome on `steps.review.done`, and the merge
            // block below is what satisfies it.)
            //
            // skip_reason cleared on boarding, in the same update that
            // stamps the train: an earlier window's skip note must not
            // outlive the skip — the key is REMOVED (Null), not left
            // behind as "". `consist_refusal` — the lint output a
            // refused consist leaves on the car it blocked — comes off
            // in the same write, for the same reason.
            //
            // `boarded_head` rides here too, and lives on the CAR
            // rather than in a second list on the train: the sweep
            // already fetches every boarded car, so the fact stays in
            // one place (guideline 9a) and costs no extra call. It is
            // rewritten on every boarding, so a car that rides a later
            // train carries that train's head, not the first one's.
            self.merge_job_metadata(
                job_id(j)?,
                vec![
                    ("train", json!(train_id.as_str())),
                    ("boarded_head", json!(head.as_str())),
                    ("skip_reason", Value::Null),
                    ("consist_refusal", Value::Null),
                ],
            )
            .await?;
        }
        log(format!(
            "train {} boarded {}, PR {pr_url}",
            id8(&train_id),
            boarded.len()
        ));
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Cancel — the operator's judgment on a train that will not arrive
    // -----------------------------------------------------------------------

    /// Cancel an open train (David's ask: trains that don't arrive
    /// were cleaned up by hand, and cancellation orphaned the cars).
    /// Car-release comes FIRST, so a crash mid-cancel leaves cars
    /// free rather than orphaned:
    ///   1. release every still-open boarded car back to the dock —
    ///      review step back to `ready` (the dock predicate requires
    ///      it; clearing metadata alone re-boards nothing),
    ///      `metadata.train` removed, `skip_reason` saying why;
    ///   2. close the PR unmerged;
    ///   3. complete the `cancelled` terminal with the reason —
    ///      jobs-api then closes the Job with outcome=cancelled and
    ///      skips the remaining steps;
    ///   4. delete the train's OWN `train/*` branch — never a car's:
    ///      the cars keep their branches (train_branch_to_delete is
    ///      the pin, and it is tested).
    /// The operator's verb. Never counts a red against the cars — an
    /// operator cancels for reasons of their own (a bad consist, a
    /// withdrawn change), and only the automatic red-stall path below
    /// has evidence that the CARS were implicated.
    async fn cancel(&self, handle: &str, reason: &str) -> Result<()> {
        self.cancel_train(handle, reason, false).await
    }

    /// Honour an operator's `cancel_requested` stamp — the yard's cancel
    /// button, read by `reconcile`. Returns whether the request has
    /// claimed this train's pass: when it has, the caller skips the rest
    /// of the pass for this train, because a train under a cancel
    /// request must not go on to merge — whether the cancel succeeded,
    /// is dry, or is being retried.
    ///
    /// NON-FATAL BY CONSTRUCTION: this returns `bool`, not `Result`, so
    /// the reconcile loop cannot `?` it. A cancel is forge writes first
    /// (`cancel_train` closes the PR before releasing a car) and the
    /// forge is the flaky half; a refusal there LOGS and the train stays
    /// intact — cars aboard, stamp in place — so the next pass retries.
    /// A fatal write in this loop once froze all landings for ~8h
    /// (boss-conductor-loop-writes-must-not-be-fatal).
    async fn honour_cancel_request(
        &self,
        train: &Value,
        tid: &str,
        pr_state: Option<&str>,
    ) -> bool {
        if let Some(refusal) = operator_cancel_refusal(train) {
            log(format!(
                "train {}: cancel requested but {refusal} — refusing, cars stay landed",
                id8(tid)
            ));
            if let Err(e) = self
                .merge_job_metadata(tid, vec![("cancel_refused", json!(refusal))])
                .await
            {
                log(format!(
                    "train {}: cancel_refused stamp failed (non-fatal, retries next pass): {e}",
                    id8(tid)
                ));
            }
            return false;
        }
        let Some(reason) = operator_cancel_reason(train) else {
            return false;
        };
        if pr_state != Some("OPEN") {
            // Neither merged nor open — closed on the forge by hand, or
            // mid-merge. The same gate the automatic rule keeps: the
            // operator verb (`boss train cancel`) takes it from here.
            log(format!(
                "train {}: cancel requested but its PR is {} — leaving it to `boss train cancel`",
                id8(tid),
                pr_state.unwrap_or("unknown")
            ));
            return false;
        }
        log(format!("train {} cancelling: {reason}", id8(tid)));
        if self.cfg.dry {
            log(format!("DRY: would cancel {} ({reason})", id8(tid)));
        } else if let Err(e) = self.cancel_train(tid, &reason, false).await {
            log(format!(
                "train {}: cancel failed (non-fatal, train intact, retries next pass): {e}",
                id8(tid)
            ));
        }
        true
    }

    async fn cancel_train(&self, handle: &str, reason: &str, count_red: bool) -> Result<()> {
        let listed = rows(
            self.api(
                Method::GET,
                "/api/jobs?kind=pr-train&status=open&limit=50",
                None,
            )
            .await?,
        )?;
        let mut trains = Vec::with_capacity(listed.len());
        for t0 in &listed {
            trains.push(self.get_job(job_id(t0)?).await?);
        }
        let train = resolve_train(&trains, handle)?;
        let tid = job_id(train)?;

        let boarded: Vec<String> = train
            .get("metadata")
            .and_then(|m| m.get("boarded_jobs"))
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        let mut cars = Vec::with_capacity(boarded.len());
        for cid in &boarded {
            cars.push(self.get_job(cid).await?);
        }
        // Say what is NOT being released, and why. A car that moved on
        // is the interesting case: silently skipping it would leave the
        // operator with a cancel that released fewer cars than the train
        // claims to carry, and no way to tell whether that was correct.
        for car in &cars {
            if car.get("status").and_then(Value::as_str) != Some("open") {
                continue;
            }
            let owner = car
                .get("metadata")
                .and_then(|m| m.get("train"))
                .and_then(Value::as_str);
            match owner {
                Some(t) if t == tid => {}
                Some(other) => log(format!(
                    "car {} now rides {} — not releasing it",
                    id8(job_id(car)?),
                    id8(other)
                )),
                None => log(format!(
                    "car {} was already released — leaving its record alone",
                    id8(job_id(car)?)
                )),
            }
        }

        let pr_url = find_step(train, "pr", "Open the batched PR")
            .and_then(|s| s.get("metadata"))
            .and_then(|m| m.get("pr_url"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        // Cancel the CI BEFORE closing the PR. Closing first leaves a
        // window where the run is still burning the single-concurrency
        // runner for a PR that is already gone, which is the state
        // 89b27e60 measured 27 minutes into.
        let train_head = train
            .get("metadata")
            .and_then(|m| m.get("train_ref"))
            .and_then(Value::as_str)
            .and_then(|r| r.rsplit('@').next())
            .unwrap_or_default()
            .to_string();
        if !pr_url.is_empty() || !train_head.is_empty() {
            // Last path segment of the PR url is its number on both
            // forges; kept inline rather than reaching for a
            // Forgejo-specific helper from forge-blind code.
            let idx = pr_url
                .trim_end_matches('/')
                .rsplit('/')
                .next()
                .unwrap_or_default()
                .to_string();
            if self.cfg.dry {
                log(format!(
                    "DRY: would cancel CI runs for PR #{idx} / {train_head}"
                ));
            } else {
                match self.forge.cancel_ci_runs(&idx, &train_head).await {
                    Ok(0) => log("cancel: no CI runs were still active".to_string()),
                    Ok(n) => log(format!("cancel: cancelled {n} in-flight CI run(s)")),
                    Err(e) => log(format!("cancel: CI cancellation failed, continuing: {e}")),
                }
            }
        }

        if !pr_url.is_empty() {
            if self.cfg.dry {
                log(format!("DRY: would close {pr_url} unmerged"));
            } else {
                self.forge.close_pr(pr_url).await?;
                log(format!("closed {pr_url} unmerged"));
            }
        }

        // RELEASE THE CARS ONLY AFTER THE FORGE WRITES SUCCEED. Cancel
        // does two kinds of write: releasing a car is a jobs-API metadata
        // write (reliable, local), closing the PR is a forge write (the
        // flaky one — an unreachable forge, a read-only token). Releasing
        // FIRST left "half-cancelled" trains: the cars back on the dock
        // but the PR still open, because close_pr's `?` returned Err with
        // the release already done (10bb1e1a; the comment at the top of
        // this file's cancel path names the two it stranded). Doing the
        // flaky writes first means a forge failure aborts here with the
        // train fully intact — cars still aboard, PR still open — so a
        // re-run is clean, and the car release only happens once the PR is
        // actually closed.
        for car in releasable_cars(&cars, tid) {
            let cid = job_id(car)?;
            // NOTHING TO REOPEN. Releasing a car is a metadata write and
            // only a metadata write, because boarding no longer completes
            // its `review` step — see the boarding loop. This used to PUT
            // the step back to `ready`, which the row silently refused
            // (terminal steps are frozen in `update_step_at`) and which
            // now 409s out loud, taking the whole cancel with it. A car
            // that predates this change still carries a completed review
            // and cannot be released; those were translated into fresh
            // packets by hand on 2026-08-15 rather than reversed.
            self.merge_job_metadata(cid, release_stamps(car, reason, count_red))
                .await?;
            log(format!("released car {} back to the dock", id8(cid)));
        }

        // The cancelled terminal is gated (blocked_by) on collect; a
        // train that died mid-assembly never completed it. Close that
        // gate honestly first — nothing boarded on the record.
        let collect = find_step(train, "collect", "Collect what is ready to board");
        if !step_done(collect) {
            self.complete_step(
                train,
                collect,
                &[(
                    "boarded",
                    Some("nothing — train cancelled before boarding completed".to_string()),
                )],
            )
            .await?;
        }
        self.complete_step(
            train,
            find_step(train, "cancelled", "Cancelled — nothing to board"),
            &[("reason", Some(reason.to_string()))],
        )
        .await?;

        if let Some(branch) = train_branch_to_delete(train) {
            if self.cfg.dry {
                log(format!(
                    "DRY: would delete branch {branch} (train cancelled)"
                ));
            } else if self.forge.delete_branch(&branch).await? {
                log(format!("deleted branch {branch} (train cancelled)"));
            } else {
                log(format!("branch {branch} already gone (train cancelled)"));
            }
        }
        log(format!("train {} cancelled: {reason}", id8(tid)));
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// THE CONDUCTOR'S LOCK — A LOSER THAT LEAVES MUST EVENTUALLY WIN
//
// One lock file serializes every conductor verb, and the loser logs and
// leaves. That is correct for a verb with a designed retry, and wrong
// for one that can be starved — which is what happened for ten hours on
// 2026-09-10 (backlog 4860aff8):
//
//   - the board cadence fires every 60 seconds and spends 12–14 seconds
//     in the consist check, so it holds the lock for a fifth of every
//     minute while a car sits on the dock;
//   - the 10-minute reconcile fires about one second after it and so
//     lost the lock EVERY time — 55 consecutive "another conductor run
//     holds the lock — leaving", each recorded `rc=0 in 0s`;
//   - the reconcile is what merges a green train, runs the stall
//     sentinel, writes arrival reports and sweeps landed branches. All
//     four were dead for nine hours, and nothing said so, because
//     nothing needed a merge in that window. It surfaced when the next
//     car tried to land — the verb that lands a fix being the verb that
//     was starved.
//
// The contention is deterministic, not a race: the board always fires
// first and always wins. So the fix is not a bigger lock, it is
// FAIRNESS — the starvable side waits a bounded time for its turn:
//
//   - RECONCILE and RUN wait (`CONDUCTOR_LOCK_WAIT`). Waiting 14 seconds
//     out of a 600-second period costs nothing, and the cadence loop's
//     one-in-flight-run-per-rule guard means a waiting pass cannot stack
//     up behind itself.
//   - BOARD and PREFLIGHT do not wait. A board must never queue behind a
//     reconcile that is deploying (job 9c5871fa: 30+ minutes), and it
//     does not need to — its firing records boarded-nothing, which
//     releases the queue-depth cooldown, and the next tick re-fires.
//
// The two alternatives were weighed and rejected. Serializing board and
// reconcile into one cadence slot (`boss train run` already does
// reconcile-then-board) fixes only the pair of rules that happen to
// collide today and leaves every other caller — a hand-run verb, a
// second conductor — starvable by the same mechanism. Shortening the
// board's hold means shortening the consist check, which is the thing
// doing the work.
//
// The budget is a constant and not delivery-policy data deliberately:
// the lock is taken before the jobs API is read, so a registry value
// would have to be fetched by a run that has not yet proved it may run.
// ---------------------------------------------------------------------------

/// How long a starvable phase waits for the conductor's lock. Two
/// minutes: comfortably longer than the 12–14s a refusing board holds it
/// and than a board that departs a train (push + PR + per-car writes),
/// and a fifth of the reconcile's own 10-minute period, so a pass that
/// waits its whole budget has still left the next window clear.
pub(crate) const CONDUCTOR_LOCK_WAIT: Duration = Duration::from_secs(120);

/// How often the waiter retries. The hold it waits out is seconds long,
/// so a one-second poll wins within a second of the lock being freed.
const LOCK_POLL: Duration = Duration::from_secs(1);

/// Preflight refused this box — distinct from a crash, loud in the
/// unit's status. Named rather than written as a bare `3` at the exit
/// site so the one test that has to know it is distinct from
/// [`LOCK_CONTENDED_EXIT`] reads the code itself, not a literal copy
/// of it.
pub(crate) const PREFLIGHT_FAIL_EXIT: i32 = 3;

/// Exit code for an invocation that gave up on the lock without doing
/// the work it came to do — either because it waited its whole budget
/// and never ran, or because it abandoned a request nobody else will
/// pick up (see [`Contended`]).
///
/// Distinct from 0 because the cadence loop records the child's exit
/// code as the firing's `rc`, and 55 starved passes recording `rc=0 in
/// 0s` is precisely how nine hours of dead maintenance read as nine
/// hours of successes — and because a `boss train cancel` that released
/// no car printed success-shaped output at 0. Distinct from
/// [`PREFLIGHT_FAIL_EXIT`] because two causes must not share one code.
pub(crate) const LOCK_CONTENDED_EXIT: i32 = 4;

/// How long this phase waits for the lock before leaving.
pub(crate) fn lock_wait_budget(phase: &Phase) -> Duration {
    match phase {
        // Starvable by construction: it fires on a fixed interval
        // against a board that fires more often and holds longer.
        Phase::Reconcile | Phase::Run => CONDUCTOR_LOCK_WAIT,
        // A board has its own retry one tick later, and must not queue
        // behind a long reconcile. Preflight proves nothing a running
        // conductor has not already proved. A cancel is an operator
        // standing at the prompt, who can see the line and re-run.
        Phase::Preflight | Phase::Board | Phase::Cancel { .. } => Duration::ZERO,
    }
}

/// Announced once, when a phase starts waiting rather than leaving.
pub(crate) fn lock_waiting_line(budget: Duration) -> String {
    format!(
        "another conductor run holds the lock — waiting up to {}s for it",
        budget.as_secs()
    )
}

/// The win. Journalled with the waited time because "how long was the
/// reconcile held off" is the number that was missing for nine hours.
pub(crate) fn lock_acquired_line(waited: Duration) -> String {
    format!(
        "took the conductor's lock after waiting {}s",
        waited.as_secs()
    )
}

/// The loss. Unchanged for a phase that does not wait — the line
/// operators and `cadence.rs`'s own doc comment already grep for — and
/// named with the waited time for one that did.
///
/// Whole seconds decide which form it takes, not `is_zero`: a phase with
/// a zero budget still spends a few hundred nanoseconds between taking
/// the clock and failing the try, and "leaving after waiting 0s" would
/// be a wait nobody waited.
pub(crate) fn lock_contended_line(waited: Duration) -> String {
    if waited.as_secs() == 0 {
        "another conductor run holds the lock — leaving".to_string()
    } else {
        format!(
            "another conductor run holds the lock — leaving after waiting {}s",
            waited.as_secs()
        )
    }
}

// ---------------------------------------------------------------------------
// Entry
// ---------------------------------------------------------------------------

pub async fn run(phase: Phase, dry: bool, now: DateTime<Utc>) -> Result<()> {
    let cfg = Config::from_env(dry);
    // The forge adapter is built before anything else — the python
    // conductor constructed FORGE at import, so a misconfigured
    // BOSS_TRAIN_FORGE fails every entry loudly, not just the boarding
    // that needed it.
    let forge = make_forge(&cfg)?;
    fs::create_dir_all(&cfg.home)?;
    let lock = File::create(Path::new(&cfg.home).join("lock"))?;
    // A held lock means a conductor run is active right now. A phase with
    // its own retry leaves at once; a starvable one waits its bounded
    // turn — see "THE CONDUCTOR'S LOCK" above.
    let budget = lock_wait_budget(&phase);
    let since = std::time::Instant::now();
    loop {
        match lock.try_lock() {
            Ok(()) => {
                // Reaching a second attempt means a poll was slept, so
                // an elapsed time of one poll or more IS a wait.
                let waited = since.elapsed();
                if waited >= LOCK_POLL {
                    log(lock_acquired_line(waited));
                }
                break;
            }
            Err(TryLockError::WouldBlock) => {
                let waited = since.elapsed();
                if waited >= budget {
                    // The line every journal reader and cadence.rs's own
                    // doc comment greps for goes out first, for every
                    // phase. A phase that abandoned a request then says
                    // WHAT it abandoned.
                    log(lock_contended_line(waited));
                    match contended(&phase) {
                        Contended::Covered => {
                            if budget > Duration::ZERO {
                                // Waited the whole budget and never ran:
                                // the firing must not record this as
                                // rc=0.
                                std::process::exit(LOCK_CONTENDED_EXIT);
                            }
                            // Leaving at once finished the job: the
                            // holder is doing this very work.
                            return Ok(());
                        }
                        // But an operator's cancel is nobody else's
                        // work. Say what did not happen, and exit
                        // non-zero so a script, a unit or a person
                        // reading the output cannot mistake it for done.
                        Contended::Abandoned(msg) => {
                            log(&msg);
                            // (The lock releases with the process;
                            // destructors are moot.)
                            std::process::exit(LOCK_CONTENDED_EXIT);
                        }
                    }
                }
                if waited < LOCK_POLL {
                    log(lock_waiting_line(budget));
                }
                tokio::time::sleep(LOCK_POLL).await;
            }
            Err(TryLockError::Error(e)) => {
                return Err(e).context("locking the conductor's lock file");
            }
        }
    }
    let problems = preflight(&cfg)?;
    if !problems.is_empty() {
        for p in &problems {
            log(format!("preflight FAIL: {p}"));
        }
        // (The lock releases with the process; destructors are moot.)
        std::process::exit(PREFLIGHT_FAIL_EXIT);
    }
    log("preflight ok");
    if matches!(phase, Phase::Preflight) {
        return Ok(());
    }
    // POLICY IS RESOLVED ONCE, HERE, and threaded from this point on.
    // One read per invocation means every decision in this run is taken
    // against one coherent set of rules, and the version is a fact the
    // journal and the train's own record can both name.
    let conductor = Conductor::new(cfg, forge)?;
    let policy = conductor.resolve_policy().await;
    let conductor = conductor.with_policy(policy);
    match phase {
        Phase::Preflight => {} // returned above; the arm keeps the match total
        Phase::Reconcile => conductor.reconcile(now).await?,
        Phase::Board => conductor.board(now).await?,
        Phase::Run => {
            // Drain publish-request packets FIRST, so a branch a
            // credential-less workspace filed this cycle is on the
            // forge before reconcile/board look — gateable in the same
            // window instead of the next one. Same clone the conductor
            // assembles in; same `fork` remote `publish_car_branch`
            // pushes car branches to.
            //
            // Same failure posture as the branch sweep above: the
            // drain is a feeder, not the train. A packet that will not
            // drain (or a jobs API that is away) is journaled and
            // retried next cycle; reconcile and board still run.
            if let Err(e) =
                crate::publish_requests::run(&conductor.cfg.clone, "fork", dry, now).await
            {
                log(format!("publish-request drain failed (run stands): {e:#}"));
            }
            conductor.reconcile(now).await?;
            conductor.board(now).await?;
        }
        Phase::Cancel { handle, reason } => conductor.cancel(&handle, &reason).await?,
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    // -- a machine cancellation says why -------------------------------

    /// Measured 2026-09-05 across every cancelled train on the record:
    /// the 11 a HUMAN cancelled all carry a reason (from `--reason`),
    /// and the 5 the MACHINE cancelled carry none. Both of the previous
    /// night's consist refusals were in the silent five, which is why a
    /// jammed yard could only be explained by reading a pod log.
    ///
    /// The terminal fires off the `empty` predicate, so a self-cancel
    /// used to complete it with no reason at all. Every abandonment now
    /// names its cause on the same field a human fills — and the causes
    /// must stay DISTINGUISHABLE, because "nothing was ready" is a
    /// healthy idle window while "a check could not run" is an outage.
    #[test]
    fn every_abandonment_reason_names_its_cause() {
        // An idle window must not read like a failure.
        let idle =
            "no car was parked and ready when the window opened — an idle window, not a failure";
        assert!(idle.contains("idle window"));
        assert!(!idle.to_lowercase().contains("refus"));

        // A consist refusal must name the check, not merely that one failed.
        let failed = vec![LintFailure {
            name: "a-kind-bundle-does-not-tighten".into(),
            files: vec!["crates/core/boss-jobs/seeds/step_types.toml".into()],
            output: "python3: command not found".into(),
        }];
        let reason = consist_refusal_reason(&failed, 200);
        assert!(
            reason.contains("a-kind-bundle-does-not-tighten"),
            "a refusal that does not name the check is one an operator must go re-derive: {reason}"
        );

        // With nothing to name, say so rather than implying a verdict.
        let empty = consist_refusal_reason(&[], 200);
        assert!(empty.contains("no failing check named"), "{empty}");
    }

    /// 2026-09-04: two gate-runs whose pods were evicted sat at
    /// `record-verdict` for 17 hours, each holding one of three gate
    /// slots and rendering as a live gate, while their branches had long
    /// since landed. A third died the same way and silently ate a car —
    /// the change was never gated and nobody noticed until a census.
    /// gate-runner.yaml already promised "a runner that dies anyway
    /// leaves an overdue packet"; nothing was listening.
    #[test]
    fn a_gate_run_past_the_job_deadline_is_dead() {
        let now = DateTime::parse_from_rfc3339("2026-09-04T13:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let run = |opened: &str, verdict_done: bool| {
            serde_json::json!({
                "id": "11111111-1111-1111-1111-111111111111",
                "metadata": { "branch": "feat/x", "opened_at": opened },
                "steps": [{
                    "spec_slug": "record-verdict",
                    "title": "Record the gate verdict",
                    "status": if verdict_done { "completed" } else { "ready" },
                    "metadata": {}
                }]
            })
        };
        // 17h with no verdict: dead, and it reports how long.
        assert_eq!(
            dead_gate_run_hours(&run("2026-09-03T20:00:00Z", false), now),
            Some(17)
        );
        // Inside the deadline it is simply a gate that is running.
        assert_eq!(
            dead_gate_run_hours(&run("2026-09-04T11:30:00Z", false), now),
            None
        );
        // A run that REPORTED is never ours to touch, however old.
        assert_eq!(
            dead_gate_run_hours(&run("2026-09-03T20:00:00Z", true), now),
            None
        );
        // No stamp, no claim — settling on a guess would write a verdict
        // nobody observed into the audit log.
        let unstamped = serde_json::json!({
            "id": "22222222-2222-2222-2222-222222222222",
            "metadata": { "branch": "feat/y" },
            "steps": [{"spec_slug":"record-verdict","title":"Record the gate verdict","status":"ready","metadata":{}}]
        });
        assert_eq!(dead_gate_run_hours(&unstamped, now), None);
    }

    use super::*;

    // ---------------------------------------------------------------
    // A cancel that cancelled nothing must not exit 0.
    // ---------------------------------------------------------------

    /// THE DEFECT (found 2026-09-10 by the builder of
    /// `fix/a-refused-consist-opens-no-train`). Losing the conductor's
    /// lock returned `Ok(())` from every phase, so `boss train cancel`
    /// printed success-shaped output and exited 0 without releasing a
    /// single car. An operator reads "cancelled", believes three cars
    /// are back on the dock, and they are still aboard a dead train
    /// holding the track — §Diagnosis's component that answered instead
    /// of erroring.
    ///
    /// Leaving AT ONCE is correct and deliberate; cancel must never
    /// wait on a contended lock. Only the report was wrong — so this
    /// pins BOTH halves, because a fix to either one alone is wrong:
    /// a zero budget that reports success is the defect, and a truthful
    /// report bought by making the operator's verb queue behind the
    /// loop is the regression.
    #[test]
    fn a_cancel_leaves_at_once_and_does_not_report_success() {
        let phase = Phase::Cancel {
            handle: "abcd1234".to_string(),
            reason: "CI red on a consist nobody can fix".to_string(),
        };
        assert_eq!(
            lock_wait_budget(&phase),
            Duration::ZERO,
            "an operator is standing at the prompt — cancel never waits"
        );
        let Contended::Abandoned(msg) = contended(&phase) else {
            panic!("a cancel that released no car has not succeeded");
        };
        // The operator must be able to read WHAT did not happen from
        // the line itself, without re-deriving it from the train.
        assert!(msg.contains("abcd1234"), "names the train: {msg}");
        assert!(msg.contains("NOT cancelled"), "names the omission: {msg}");
        assert!(msg.contains("lock"), "names the cause: {msg}");
    }

    /// The standing loop's phases are a different case, and stay quiet.
    /// The lock holder is running reconcile + board right now, so the
    /// work this invocation came to do is being done by the process
    /// that beat it here; a preflight has nothing further to prove
    /// while the locomotive is demonstrably pulling. Nothing else in
    /// the system will cancel an operator's named train.
    ///
    /// `Covered` is about abandonment, not about the exit code: a
    /// reconcile that spent its whole budget is still covered by the
    /// holder, and exits [`LOCK_CONTENDED_EXIT`] because it never ran.
    #[test]
    fn the_standing_loop_leaves_quietly_because_the_holder_covers_it() {
        for phase in [Phase::Preflight, Phase::Reconcile, Phase::Board, Phase::Run] {
            assert_eq!(contended(&phase), Contended::Covered);
        }
    }

    // ---------------------------------------------------------------
    // The adapter must match the remote (b9801aff).
    // ---------------------------------------------------------------

    /// THE EXACT MISCONFIGURATION. `BOSS_TRAIN_FORGE` unset defaults to
    /// `github`, and the conductor clone's origin is the internal forge.
    /// Two trains were left half-cancelled because this was only
    /// discovered by `gh` failing AFTER the cars were released.
    #[test]
    fn the_github_adapter_over_a_forge_origin_is_refused() {
        let p =
            forge_mismatch("github", "http://10.20.0.15:3000/david/boss.git").expect("must refuse");
        assert!(p.contains("BOSS_TRAIN_FORGE=forgejo"), "{p}");
        assert!(p.contains("AFTER releasing its cars"), "{p}");
    }

    /// The mirror image, so the check is not just a github-shaped grep.
    #[test]
    fn the_forgejo_adapter_over_github_is_refused() {
        assert!(forge_mismatch("forgejo", "https://github.com/algedonic-dev/boss.git").is_some());
    }

    /// AND THE FALSE POSITIVES THAT WOULD STOP THE CONDUCTOR. Each of
    /// these is a working configuration; refusing any of them would be
    /// worse than the bug, because preflight gates every train.
    #[test]
    fn matching_configurations_are_left_alone() {
        assert_eq!(
            forge_mismatch("forgejo", "http://10.20.0.15:3000/david/boss.git"),
            None
        );
        assert_eq!(
            forge_mismatch("github", "https://github.com/algedonic-dev/boss.git"),
            None
        );
        assert_eq!(
            forge_mismatch("github", "git@github.com:david/boss.git"),
            None
        );
        // THE FALSE POSITIVE THE GATE CAUGHT. `healthy_clone_passes`
        // points origin at a local bare repo with no forge configured,
        // and the first version of this check called that a
        // misconfiguration — failing a fixture that is entirely healthy.
        // A filesystem path addresses no host, so it cannot contradict
        // an adapter.
        //
        // shared-tmp-ok: an expectation string about a remote URL, not a
        // path anything builds — nothing here touches the filesystem.
        assert_eq!(
            forge_mismatch("github", "/tmp/boss-preflight-102054-healthy/upstream.git"),
            None
        );
        assert_eq!(forge_mismatch("forgejo", "/srv/git/boss.git"), None);
        assert_eq!(forge_mismatch("github", "../fixtures/upstream.git"), None);
        // An unreadable origin is not a contradiction, and an unknown
        // adapter is make_forge's error to raise, not preflight's.
        assert_eq!(forge_mismatch("github", ""), None);
        assert_eq!(
            forge_mismatch("gitlab", "http://10.20.0.15:3000/x.git"),
            None
        );
    }

    // ---------------------------------------------------------------
    // Re-railing — the conductor's answer to a squash-merged base.
    // ---------------------------------------------------------------

    /// The exact shape that cost four hand re-rails on 2026-08-15: a
    /// car whose work is partly in main already, because the branch it
    /// was cut from was SQUASH-merged and so is not an ancestor of
    /// main. Merging re-applies the landed hunk onto itself; rebasing
    /// recognises it as already applied and drops it.
    #[test]
    fn a_car_whose_base_was_squash_merged_is_re_railed_not_skipped() {
        let dir = std::env::temp_dir().join(format!("boss-rerail-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let d = dir.to_str().unwrap();
        let git = |args: &[&str]| {
            let mut a = vec!["git", "-C", d];
            a.extend_from_slice(args);
            let out = sh_unchecked(&a).unwrap();
            assert!(out.status.success(), "git {args:?}: {}", stdout_str(&out));
        };
        let write = |name: &str, body: &str| {
            std::fs::write(dir.join(name), body).unwrap();
        };

        git(&["init", "-q", "-b", "main"]);
        git(&["config", "user.email", "t@t"]);
        git(&["config", "user.name", "t"]);
        write("base.txt", "base\n");
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "base"]);

        // The parent car adds a line; the child car is cut from it and
        // adds another to the SAME file.
        git(&["checkout", "-q", "-b", "parent"]);
        write("shared.txt", "from parent\n");
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "parent work"]);
        git(&["checkout", "-q", "-b", "child"]);
        write("shared.txt", "from parent\nfrom child\n");
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "child work"]);

        // The parent lands as a SQUASH — new sha, same content, and
        // `parent` is now not an ancestor of main.
        git(&["checkout", "-q", "main"]);
        git(&["merge", "-q", "--squash", "parent"]);
        git(&["commit", "-q", "-m", "train: parent (squashed)"]);

        // The conductor's world: fork/<branch> refs and a train branch
        // cut from main.
        git(&["update-ref", "refs/remotes/fork/child", "child"]);
        git(&["checkout", "-q", "-B", "train", "main"]);

        // A plain merge collides on the line the squash already landed.
        let merged = sh_unchecked(&[
            "git",
            "-C",
            d,
            "merge",
            "--no-ff",
            "-m",
            "train: merge child",
            "fork/child",
        ])
        .unwrap();
        assert!(
            !merged.status.success(),
            "the bug only exists because this merge conflicts"
        );
        sh_unchecked(&["git", "-C", d, "merge", "--abort"]).unwrap();

        // Re-railing replays only the child's own commit and lands it.
        let rerailed = rerail_onto_consist(d, "train", "child")
            .unwrap()
            .expect("a squash-merged base is exactly what rebase resolves");
        let retry = sh_unchecked(&[
            "git",
            "-C",
            d,
            "merge",
            "--no-ff",
            "-m",
            "train: merge child (re-railed)",
            &rerailed,
        ])
        .unwrap();
        assert!(retry.status.success(), "re-railed car must merge cleanly");

        let body = std::fs::read_to_string(dir.join("shared.txt")).unwrap();
        assert_eq!(
            body, "from parent\nfrom child\n",
            "the child's work lands on top of the parent's, once"
        );

        // And the clone is left on the train branch, ready for the next
        // car in the loop.
        let head = sh_unchecked(&["git", "-C", d, "rev-parse", "--abbrev-ref", "HEAD"]).unwrap();
        assert_eq!(stdout_str(&head).trim(), "train");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A car that genuinely disagrees with the consist still gets
    /// skipped — re-railing must not paper over a real conflict.
    #[test]
    fn a_real_conflict_still_refuses_to_re_rail() {
        let dir = std::env::temp_dir().join(format!("boss-rerail-real-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let d = dir.to_str().unwrap();
        let git = |args: &[&str]| {
            let mut a = vec!["git", "-C", d];
            a.extend_from_slice(args);
            let out = sh_unchecked(&a).unwrap();
            assert!(out.status.success(), "git {args:?}: {}", stdout_str(&out));
        };

        git(&["init", "-q", "-b", "main"]);
        git(&["config", "user.email", "t@t"]);
        git(&["config", "user.name", "t"]);
        std::fs::write(dir.join("f.txt"), "original\n").unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "base"]);

        git(&["checkout", "-q", "-b", "car"]);
        std::fs::write(dir.join("f.txt"), "the car's answer\n").unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "car"]);

        git(&["checkout", "-q", "main"]);
        std::fs::write(dir.join("f.txt"), "a different answer\n").unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "someone else"]);

        git(&["update-ref", "refs/remotes/fork/car", "car"]);
        git(&["checkout", "-q", "-B", "train", "main"]);

        assert!(
            rerail_onto_consist(d, "train", "car").unwrap().is_none(),
            "two answers to the same line is a conflict a human owns"
        );
        let head = sh_unchecked(&["git", "-C", d, "rev-parse", "--abbrev-ref", "HEAD"]).unwrap();
        assert_eq!(
            stdout_str(&head).trim(),
            "train",
            "a failed re-rail must still leave the clone usable"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
    use super::{
        ApiFailure, CarBranch, ConvergenceVerdict, Failure, JOBS_API_RETRY,
        NO_PLAYGROUND_DEPLOY_EVIDENCE, RetryPolicy, SweepGuard, arrival_already_filed,
        arrival_report, arrival_summary, auto_cancel_reason, boarded_head, branch_moved_line,
        car_hold_reason, ci_overdue, claim_deferred_branches, classify_transport, commits_match,
        convergence_verdict, deletable_branches, deploy_needed, local_jobs_problem,
        merge_declined_reason, overlay_metadata, parked_ready, playground_deploy_disabled,
        releasable_cars, repo_path, resolve_train, retryable, retrying, short_cause,
        skip_reason_branch_missing, skip_reason_conflict, stall_age_hours, sweep_complete,
        sweep_guard, sweep_note, sweep_pending, sweep_settled, sweep_subject,
        train_branch_to_delete, verdict_drift,
    };
    use crate::delivery_policy::DeliveryPolicy;
    use anyhow::{Result, anyhow};
    use chrono::{DateTime, Utc};
    use reqwest::Method;
    use serde_json::{Value, json};
    use std::collections::BTreeSet;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::Duration;

    /// THE POLICY EVERY TEST BELOW DECIDES BY, unless it is deliberately
    /// exercising a different one. It is the compiled fallback, which is
    /// exactly what the seeded registry row parses to
    /// (`delivery_policy::db_tests::the_seeded_policy_equals_the_compiled_fallback`)
    /// — so these tests pin the same behaviour they pinned before the
    /// numbers moved.
    fn policy() -> DeliveryPolicy {
        DeliveryPolicy::compiled()
    }

    /// A car parked at review, branch pushed, not on a train.
    fn ready_car() -> serde_json::Value {
        json!({
            "id": "car-1",
            "metadata": {"branch": "feat/x"},
            "steps": [
                {"spec_slug": "review", "title": "Open for review", "status": "ready"}
            ],
        })
    }

    /// The receipt spot-check (742d1faa, David: "Agreed"): the two
    /// lies it exists to catch, the unverifiable cases, and the one
    /// honest pass.
    fn car_with_receipt(receipt: serde_json::Value) -> serde_json::Value {
        let mut c = ready_car();
        c["steps"].as_array_mut().unwrap().push(json!({
            "spec_slug": "gate",
            "title": "Green, and observed working",
            "status": "completed",
            "metadata": {"receipt": receipt.to_string()},
        }));
        c
    }

    /// A RE-GATE RECEIPT IS THE ONE THAT COUNTS.
    ///
    /// User feedback 64cae7e9: a car whose branch legitimately moved was
    /// unrepairable, because the gate step is immutable and its receipt
    /// correctly stops vouching for the new head. Two cars were abandoned
    /// on 2026-08-28 for exactly this. With `regate_receipt` on the car,
    /// the fresh receipt is read and the car boards — no new packet, and
    /// the car keeps its history.
    #[test]
    fn a_regate_receipt_supersedes_a_stale_gate_receipt() {
        let stale = r#"{"verdict":"green","head":"1111111111111111111111111111111111111111"}"#;
        let fresh = r#"{"verdict":"green","head":"2222222222222222222222222222222222222222"}"#;
        let car = json!({
            "metadata": {"regate_receipt": fresh},
            "steps": [{"spec_slug": "gate", "title": "Green, and observed working",
                       "status": "completed", "metadata": {"receipt": stale}}],
        });
        assert_eq!(
            receipt_skip_reason(&car, Some("2222222222222222222222222222222222222222")),
            None,
            "the re-gate vouches for the head being boarded; the car should board"
        );
    }

    /// Without a re-gate the original still rules, so a stale car is
    /// still refused and the reason still names the mismatch.
    #[test]
    fn no_regate_leaves_the_original_receipt_in_force() {
        let stale = r#"{"verdict":"green","head":"1111111111111111111111111111111111111111"}"#;
        let car = json!({
            "metadata": {},
            "steps": [{"spec_slug": "gate", "title": "Green, and observed working",
                       "status": "completed", "metadata": {"receipt": stale}}],
        });
        let reason = receipt_skip_reason(&car, Some("2222222222222222222222222222222222222222"))
            .expect("a stale receipt with no re-gate must still be refused");
        assert!(reason.contains("gated, then changed"), "{reason}");
    }

    /// A re-gate is not a way to launder a red run.
    #[test]
    fn a_regate_that_is_not_green_is_refused_like_any_other() {
        let stale = r#"{"verdict":"green","head":"1111111111111111111111111111111111111111"}"#;
        let red = r#"{"verdict":"failed","head":"2222222222222222222222222222222222222222"}"#;
        let car = json!({
            "metadata": {"regate_receipt": red},
            "steps": [{"spec_slug": "gate", "title": "Green, and observed working",
                       "status": "completed", "metadata": {"receipt": stale}}],
        });
        let reason = receipt_skip_reason(&car, Some("2222222222222222222222222222222222222222"))
            .expect("a failed re-gate must not board");
        assert!(reason.contains("not green"), "{reason}");
    }

    #[test]
    fn an_honest_receipt_boards() {
        let c = car_with_receipt(json!({
            "verdict": "green", "dirty": false,
            "head": "abcdef1234567890abcdef1234567890abcdef12",
        }));
        assert_eq!(
            receipt_skip_reason(&c, Some("abcdef1234567890abcdef1234567890abcdef12")),
            None
        );
        // Prefix-tolerant like every other head comparison here.
        assert_eq!(receipt_skip_reason(&c, Some("abcdef12345678")), None);
    }

    #[test]
    fn a_receipt_for_a_different_head_is_named_and_left_behind() {
        let c = car_with_receipt(json!({
            "verdict": "green", "dirty": false,
            "head": "abcdef1234567890abcdef1234567890abcdef12",
        }));
        let reason = receipt_skip_reason(&c, Some("1234567890abcdef1234567890abcdef12345678"))
            .expect("gated-then-changed must be caught");
        assert!(reason.contains("gated, then changed"), "{reason}");
    }

    #[test]
    fn a_non_green_or_dirty_receipt_is_left_behind() {
        let red = car_with_receipt(json!({"verdict": "failed", "dirty": false, "head": "abc"}));
        assert!(
            receipt_skip_reason(&red, Some("abc"))
                .expect("red must be caught")
                .contains("not green")
        );
        let dirty = car_with_receipt(json!({"verdict": "green", "dirty": true, "head": "abc"}));
        assert!(
            receipt_skip_reason(&dirty, Some("abc"))
                .expect("dirty must be caught")
                .contains("dirty tree")
        );
    }

    #[test]
    fn a_missing_receipt_is_unverifiable_not_a_pass() {
        let mut c = ready_car();
        c["steps"].as_array_mut().unwrap().push(json!({
            "spec_slug": "gate", "title": "Green, and observed working",
            "status": "completed", "metadata": {},
        }));
        assert!(
            receipt_skip_reason(&c, Some("abc"))
                .expect("no receipt must be caught")
                .contains("unverifiable")
        );
        // An OBJECT-shaped receipt is a receipt too.
        let mut obj = ready_car();
        obj["steps"].as_array_mut().unwrap().push(json!({
            "spec_slug": "gate", "title": "Green, and observed working",
            "status": "completed",
            "metadata": {"receipt": {"verdict": "green", "dirty": false, "head": "abc12345"}},
        }));
        assert_eq!(receipt_skip_reason(&obj, Some("abc12345")), None);
    }

    /// BOTH RECEIPT SHAPES BOARD, because both are on real cars.
    ///
    /// Until 2026-09-09 the gate runner reduced `infra/gate.sh`'s account
    /// of a run to `{verdict, head, mode, fails}` before reporting it, so
    /// every receipt on a landed car has exactly those four keys and this
    /// check's `dirty` clause never had a value to read. The runner now
    /// reports the whole receipt — mode, scope, dirty, host, ci, free_gb,
    /// unverifiable, every check with its duration, and the report-back's
    /// own story. Widening a record two verbs already parse is where this
    /// breaks, so the two shapes are pinned side by side: the extra keys
    /// must be ignored, and the four-field receipts already on the dock
    /// must keep boarding.
    #[test]
    fn a_wide_receipt_and_a_four_field_one_both_board() {
        const HEAD: &str = "abcdef1234567890abcdef1234567890abcdef12";
        let old = car_with_receipt(json!({
            "verdict": "green", "head": HEAD, "mode": "full", "fails": [],
        }));
        assert_eq!(
            receipt_skip_reason(&old, Some(HEAD)),
            None,
            "a receipt recorded before the runner stopped reducing must still board"
        );
        let wide = car_with_receipt(json!({
            "verdict": "green", "mode": "full", "scope": "", "head": HEAD,
            "dirty": false, "host": "gate-runner-abc", "ci": true, "free_gb": 91,
            "unverifiable": [],
            "report": {"attempts": 4, "waited_s": 63, "sor_unreachable": true},
            "checks": [{"name": "fmt", "result": "pass", "seconds": 3},
                       {"name": "test", "result": "pass", "seconds": 812}],
        }));
        assert_eq!(
            receipt_skip_reason(&wide, Some(HEAD)),
            None,
            "the fields the whole receipt adds must be ignored, not refused"
        );
    }

    /// ...and the clause the widening WAKES UP still refuses.
    ///
    /// `dirty` has been in gate.sh's receipt from the start and has never
    /// once reached this function, because the runner dropped it. It
    /// arrives now, so the refusal it was written for becomes live for
    /// the first time on a real car — pinned here deliberately rather
    /// than discovered on a train.
    #[test]
    fn a_wide_receipt_taken_on_a_dirty_tree_is_still_refused() {
        const HEAD: &str = "abcdef1234567890abcdef1234567890abcdef12";
        let dirty = car_with_receipt(json!({
            "verdict": "green", "mode": "full", "head": HEAD, "dirty": true,
            "checks": [{"name": "fmt", "result": "pass", "seconds": 3}],
        }));
        let reason = receipt_skip_reason(&dirty, Some(HEAD))
            .expect("a receipt taken on a dirty tree must not board");
        assert!(reason.contains("dirty tree"), "{reason}");
    }

    // -- the conductor reads all its cars, not just page one -----------

    #[test]
    fn next_offset_pages_past_the_first_hundred() {
        // A backlog under one page needs no second read.
        assert_eq!(next_offset(0, 0), None);
        assert_eq!(next_offset(42, 42), None);
        assert_eq!(next_offset(PAGE_LIMIT, PAGE_LIMIT), None);
        // 150 open cars, 100 read: page two starts at offset 100.
        assert_eq!(next_offset(150, 100), Some(100));
        // page two read: the whole backlog is covered.
        assert_eq!(next_offset(150, 150), None);
        // defensive — a `total` that shrank mid-read never asks for more.
        assert_eq!(next_offset(150, 160), None);
    }

    #[test]
    fn list_total_reads_total_not_the_page() {
        let body = json!({"data": [{"id": "car-1"}], "limit": 100, "offset": 0, "total": 150});
        assert_eq!(list_total(&body).unwrap(), 150);
        // no `total` is an error, never zero: zero is a wrong deployment.
        assert!(list_total(&json!({"data": []})).is_err());
    }

    /// THE STARVATION REGRESSION. A parked-ready car opened days ago
    /// sorts to the tail of `opened_on DESC`; with 150 open cars it sits
    /// on page two (offset 100). The old bare `limit=100` read left it
    /// off page one forever. `list_all_pages` — the read every whole-dock
    /// caller now shares (candidates, open_car_branches, preview_dock,
    /// probe_dock_depth) — must gather it.
    #[tokio::test]
    async fn list_all_pages_reads_the_car_on_page_two() {
        let all: Vec<Value> = (0..150)
            .map(|i| json!({"id": format!("car-{i}")}))
            .collect();
        let all_ref = &all;
        let calls = std::cell::Cell::new(0u32);
        let gathered = list_all_pages(|offset| {
            calls.set(calls.get() + 1);
            async move {
                let page: Vec<Value> = all_ref
                    .iter()
                    .skip(offset)
                    .take(PAGE_LIMIT)
                    .cloned()
                    .collect();
                anyhow::Ok(Some(json!({
                    "data": page,
                    "total": all_ref.len(),
                    "offset": offset,
                    "limit": PAGE_LIMIT,
                })))
            }
        })
        .await
        .unwrap();
        assert_eq!(gathered.len(), 150, "every open car must be read");
        assert!(
            gathered.iter().any(|j| j["id"] == "car-149"),
            "the car on page two must be gathered, not left off page one"
        );
        assert_eq!(calls.get(), 2, "150 cars is two pages of 100");
    }

    /// A backlog that fits under a page still makes exactly one call —
    /// the fix is paging PAST a page, never changing behaviour below it.
    #[tokio::test]
    async fn list_all_pages_makes_one_call_below_a_page() {
        let calls = std::cell::Cell::new(0u32);
        let gathered = list_all_pages(|offset| {
            calls.set(calls.get() + 1);
            async move {
                assert_eq!(offset, 0, "a sub-page backlog never asks for page two");
                anyhow::Ok(Some(json!({
                    "data": (0..42).map(|i| json!({"id": i})).collect::<Vec<_>>(),
                    "total": 42,
                    "offset": offset,
                    "limit": PAGE_LIMIT,
                })))
            }
        })
        .await
        .unwrap();
        assert_eq!(gathered.len(), 42);
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn a_car_at_review_with_a_branch_is_parked_ready() {
        assert!(parked_ready(&ready_car()));
        let mut active = ready_car();
        active["steps"][0]["status"] = json!("active");
        assert!(parked_ready(&active));
    }

    #[test]
    fn a_held_car_does_not_board_and_does_not_count() {
        // The hold is the whole point: this car is at review, has a
        // branch, and is gated green. It still must not ride, because
        // something about the WORLD is wrong - on 2026-08-26, the only
        // node carrying the label a car repointed the gate rig at had
        // been cordoned for a hardware fault.
        let mut held = ready_car();
        held["steps"][0]["metadata"] = json!({"hold": "w-1 is cordoned"});
        assert!(!parked_ready(&held));

        // `active` is boardable too, so it must honour the hold as well.
        let mut held_active = ready_car();
        held_active["steps"][0]["status"] = json!("active");
        held_active["steps"][0]["metadata"] = json!({"hold": "not yet"});
        assert!(!parked_ready(&held_active));

        // A RELEASED hold is not a hold, and it is released by writing a
        // falsy value, not by deleting the key - `hold: false` is what
        // releasing car 04520403 wrote on 2026-09-10, and `""`/`null`
        // arrive the same way. `stranded::marked` reads all three as no
        // marker; a rule built on "the key is missing" would hold these
        // cars forever.
        for released in [json!(""), json!(false), json!(null), json!("   ")] {
            let mut car = ready_car();
            car["steps"][0]["metadata"] = json!({"hold": released});
            assert!(parked_ready(&car), "a released hold must board: {released}");
        }

        // Metadata that says nothing about holding leaves it boardable.
        let mut other = ready_car();
        other["steps"][0]["metadata"] = json!({"note": "looks fine"});
        assert!(parked_ready(&other));
    }

    #[test]
    fn a_car_without_a_branch_is_not_ready() {
        let mut j = ready_car();
        j["metadata"] = json!({});
        assert!(!parked_ready(&j));
        j["metadata"] = json!({"branch": ""});
        assert!(!parked_ready(&j));
    }

    #[test]
    fn a_car_already_on_a_train_is_not_ready() {
        let mut j = ready_car();
        j["metadata"]["train"] = json!("train-job-id");
        assert!(!parked_ready(&j));
        // A train's own branch is never a car either.
        let mut t = ready_car();
        t["metadata"]["branch"] = json!("train/20260812-0600");
        assert!(!parked_ready(&t));
    }

    // A red train and a green one must not produce the same line.
    // Trains 46 and 47 both went red on 2026-08-16 and the journal
    // said `completed ci on <id>` for each — the same text train 45
    // produced going green.
    #[test]
    fn a_completed_step_says_what_it_recorded() {
        let red = completion_log_line(
            "ci",
            "e78859ab",
            &[("result", Some("failing".into())), ("notify_on_done", None)],
        );
        let green = completion_log_line(
            "ci",
            "810a7a3f",
            &[("result", Some("green".into())), ("notify_on_done", None)],
        );
        assert_ne!(
            red.replace("e78859ab", "X"),
            green.replace("810a7a3f", "X"),
            "red and green must be distinguishable without knowing the train id"
        );
        assert!(red.contains("failing"), "{red}");
        // A field with nothing in it is not evidence, and printing it
        // as None trains the reader to stop at the id.
        assert!(!red.contains("None"), "{red}");
    }

    // Most steps complete with no evidence; those lines stay as they
    // were rather than gaining an empty dict.
    #[test]
    fn a_step_with_no_evidence_logs_as_before() {
        assert_eq!(
            completion_log_line("assemble", "e78859ab", &[]),
            "completed assemble on e78859ab"
        );
        assert_eq!(
            completion_log_line("assemble", "e78859ab", &[("skipped", None)]),
            "completed assemble on e78859ab"
        );
    }

    #[test]
    fn a_released_car_is_parked_ready_again() {
        // THE INVARIANT THAT MAKES CANCELLING A LOADED TRAIN POSSIBLE.
        // Releasing a car clears `train` and nothing else, so the car
        // must be boardable on that write alone. It is — as long as
        // boarding left the review step ready.
        let mut boarded = ready_car();
        boarded["metadata"]["train"] = json!("train-job-id");
        boarded["metadata"]["boarded_head"] = json!("abc1234");
        assert!(!parked_ready(&boarded));

        let mut released = boarded.clone();
        released["metadata"]["train"] = Value::Null;
        released["metadata"]["boarded_head"] = Value::Null;
        assert!(
            parked_ready(&released),
            "a released car must re-enter the dock on the metadata write alone"
        );

        // And the reason boarding must NOT complete the step: a
        // completed review is frozen at the row, so a released car
        // carrying one could never board again and the cancel would be
        // a lie.
        let mut released_but_reviewed = released.clone();
        released_but_reviewed["steps"][0]["status"] = json!("completed");
        assert!(!parked_ready(&released_but_reviewed));
    }

    #[test]
    fn a_car_not_yet_at_review_is_not_ready() {
        let mut j = ready_car();
        j["steps"][0]["status"] = json!("pending");
        assert!(!parked_ready(&j));
        j["steps"][0]["status"] = json!("completed");
        assert!(!parked_ready(&j));
        // No review step at all.
        j["steps"] = json!([]);
        assert!(!parked_ready(&j));
    }

    // -- the branch-sweep decision at arrival ------------------------------
    //
    // Train PRs squash-merge, so git ancestry can never prove a car's
    // content landed; the JOB RECORD is the proof (protocol decision,
    // David). These pin exactly which branches the conductor may
    // delete once a train has arrived.

    /// A boarded car whose bookkeeping completed: closed with the
    /// `merged` outcome stamped by the terminal close.
    fn landed_car(id: &str, branch: &str) -> serde_json::Value {
        json!({
            "id": id,
            "status": "closed",
            "metadata": {"branch": branch, "outcome": "merged", "merged": "true"},
        })
    }

    fn no_open() -> BTreeSet<String> {
        BTreeSet::new()
    }

    /// The branch names a sweep decision offers, in order.
    fn names(decided: &[CarBranch]) -> Vec<&str> {
        decided.iter().map(|b| b.branch.as_str()).collect()
    }

    #[test]
    fn a_landed_cars_branch_is_deletable() {
        let cars = vec![landed_car("car-1", "feat/x")];
        let decided = deletable_branches(&cars, &no_open());
        assert_eq!(names(&decided), vec!["feat/x"]);
        assert_eq!(decided[0].car, "car-1");
        assert!(
            !decided[0].rerail_origin,
            "the car's own branch is not a rerail original"
        );
    }

    #[test]
    fn a_car_still_open_keeps_its_branch() {
        // Bookkeeping incomplete — the dispatcher has not closed the
        // car yet, whatever the train did.
        let mut car = landed_car("car-1", "feat/x");
        car["status"] = json!("open");
        assert!(deletable_branches(&[car], &no_open()).is_empty());
    }

    #[test]
    fn an_abandoned_car_keeps_its_branch() {
        // Abandoned cars close too — but their branch holds unmerged
        // work. Only the `merged` outcome is landing evidence.
        let mut car = landed_car("car-1", "feat/x");
        car["metadata"]["outcome"] = json!("abandoned");
        assert!(deletable_branches(&[car], &no_open()).is_empty());
    }

    #[test]
    fn a_closed_car_without_an_outcome_keeps_its_branch() {
        // Closed by hand, no terminal outcome on the record: not proof.
        let mut car = landed_car("car-1", "feat/x");
        car["metadata"].as_object_mut().unwrap().remove("outcome");
        assert!(deletable_branches(&[car], &no_open()).is_empty());
    }

    #[test]
    fn main_is_never_deletable() {
        let cars = vec![landed_car("car-1", "main")];
        assert!(deletable_branches(&cars, &no_open()).is_empty());
    }

    #[test]
    fn a_branch_a_still_open_car_names_survives() {
        // A follow-up car may ride a landed car's branch; the open
        // car's claim wins.
        let open: BTreeSet<String> = ["feat/x".to_string()].into();
        let cars = vec![landed_car("car-1", "feat/x")];
        assert!(deletable_branches(&cars, &open).is_empty());
    }

    #[test]
    fn a_car_without_a_branch_contributes_nothing() {
        let empty = landed_car("car-1", "");
        assert!(deletable_branches(&[empty], &no_open()).is_empty());
        let mut none = landed_car("car-2", "feat/x");
        none["metadata"] = json!({"outcome": "merged"});
        assert!(deletable_branches(&[none], &no_open()).is_empty());
    }

    #[test]
    fn two_landed_cars_on_one_branch_delete_it_once() {
        let cars = vec![landed_car("car-1", "feat/x"), landed_car("car-2", "feat/x")];
        let decided = deletable_branches(&cars, &no_open());
        assert_eq!(names(&decided), vec!["feat/x"]);
        assert_eq!(decided[0].car, "car-1", "the first car named it");
    }

    // -- the rerail original (packet 473fda1b, generator 1) ---------------
    //
    // `boss rerail` moves a car to a NEW branch and leaves the original
    // on the forge. The train merges and deletes the branch it carried
    // — the rerail one — and the original was never a car, so a sweep
    // that iterates boarded cars has nothing to act on and will never
    // consider it: permanent by construction, 13 branches deep by
    // 2026-09-10. The fix reads the provenance the rerail RECORDED on
    // the car (`rerail_origins`), never a `-rerail` suffix guessed off
    // a name.

    /// The record `boss rerail` leaves: the car now rides `branch`, and
    /// each branch it was re-railed off is named with the head that
    /// branch carried at the moment the car left it.
    fn rerailed_car(id: &str, branch: &str, origins: &[(&str, &str)]) -> Value {
        let mut car = landed_car(id, branch);
        car["metadata"]["boarded_head"] = json!(format!("head-of-{branch}"));
        car["metadata"]["rerail_origins"] = json!(
            origins
                .iter()
                .map(|(b, h)| json!({"branch": b, "head": h}))
                .collect::<Vec<_>>()
        );
        car
    }

    #[test]
    fn a_landed_rerail_cars_original_branch_is_deletable_too() {
        // The pure case from the packet: the work landed under the
        // `-rerail` twin, and nothing would ever have swept the original.
        let cars = vec![rerailed_car(
            "car-1",
            "feat/x-rerail",
            &[("feat/x", "head-of-feat/x")],
        )];
        assert_eq!(
            names(&deletable_branches(&cars, &no_open())),
            vec!["feat/x-rerail", "feat/x"],
            "the original must be swept alongside the twin that carried it"
        );
    }

    #[test]
    fn a_rerail_original_a_live_car_claims_is_deferred_not_deleted() {
        // Someone re-used the original branch name for new work: the
        // live car's claim beats any landed car's deletion, and the
        // deferral keeps the train pending rather than stamping over it.
        let cars = vec![rerailed_car(
            "car-1",
            "feat/x-rerail",
            &[("feat/x", "head-of-feat/x")],
        )];
        let claimed: BTreeSet<String> = ["feat/x".to_string()].into_iter().collect();
        assert_eq!(
            names(&deletable_branches(&cars, &claimed)),
            vec!["feat/x-rerail"],
            "the claimed original survives; the twin is still swept"
        );
        assert_eq!(
            names(&claim_deferred_branches(&cars, &claimed)),
            vec!["feat/x"],
            "and the deferral is named, not silent"
        );
        assert!(
            !sweep_complete(0, claim_deferred_branches(&cars, &claimed).len(), &cars),
            "a deferred original must not be stamped over"
        );
    }

    #[test]
    fn an_abandoned_cars_rerail_original_keeps_its_branch() {
        // Abandonment is a DISPOSITION, not a sweep (packet 473fda1b):
        // the branch holds the only copy of work someone chose to stop,
        // and so does the branch it was re-railed off.
        let mut car = rerailed_car("car-1", "feat/x-rerail", &[("feat/x", "head-of-feat/x")]);
        car["metadata"]["outcome"] = json!("abandoned");
        assert!(
            deletable_branches(&[car], &no_open()).is_empty(),
            "neither the twin nor the original is landing evidence"
        );
    }

    #[test]
    fn a_car_re_railed_twice_offers_each_original_once() {
        // A second conflict re-rails an already-re-railed car, so the
        // chain is two deep. Every branch in it landed its content under
        // the car's current head — and an origin that repeats the car's
        // own branch must not be offered twice.
        let cars = vec![rerailed_car(
            "car-1",
            "feat/x-rerail-rerail",
            &[
                ("feat/x", "head-of-feat/x"),
                ("feat/x-rerail", "head-of-feat/x-rerail"),
                ("feat/x-rerail-rerail", "head-of-feat/x-rerail-rerail"),
            ],
        )];
        assert_eq!(
            names(&deletable_branches(&cars, &no_open())),
            vec!["feat/x-rerail-rerail", "feat/x", "feat/x-rerail"],
            "both originals, the car's own branch once, nothing invented"
        );
    }

    #[test]
    fn main_is_never_deletable_even_as_a_rerail_origin() {
        let cars = vec![rerailed_car(
            "car-1",
            "feat/x-rerail",
            &[("main", "head-of-main")],
        )];
        assert_eq!(
            names(&deletable_branches(&cars, &no_open())),
            vec!["feat/x-rerail"],
            "a malformed origin naming main must never reach the forge call"
        );
    }

    #[test]
    fn an_origin_without_a_branch_name_contributes_nothing() {
        let mut car = rerailed_car("car-1", "feat/x-rerail", &[]);
        car["metadata"]["rerail_origins"] = json!([{"head": "abc"}, {"branch": ""}, "feat/x"]);
        assert_eq!(
            names(&deletable_branches(&[car], &no_open())),
            vec!["feat/x-rerail"],
            "a malformed origins list costs the car's own sweep nothing"
        );
    }

    #[test]
    fn the_sweep_settles_only_when_every_boarded_car_is_terminal() {
        let landed = landed_car("car-1", "feat/x");
        let mut still_open = landed_car("car-2", "feat/y");
        still_open["status"] = json!("open");
        let mut cancelled = landed_car("car-3", "feat/z");
        cancelled["status"] = json!("cancelled");
        assert!(sweep_settled(std::slice::from_ref(&landed)));
        assert!(sweep_settled(&[landed.clone(), cancelled]));
        assert!(!sweep_settled(&[landed, still_open]));
        // Nothing boarded is trivially settled.
        assert!(sweep_settled(&[]));
    }

    // -- the sweep's coverage (packet 02069932) -----------------------------

    fn closed_train(id: &str, boarded: bool, swept: bool) -> Value {
        let mut md = serde_json::Map::new();
        if boarded {
            md.insert("boarded_jobs".into(), json!(["car-1"]));
        } else {
            md.insert("outcome".into(), json!("cancelled"));
        }
        if swept {
            md.insert("branches_swept".into(), json!("true"));
        }
        json!({"id": id, "kind": "pr-train", "status": "closed", "metadata": md})
    }

    /// THE COVERAGE LEAK. The sweep read the newest 50 closed trains
    /// under a comment claiming coverage was never capped. A refusing
    /// consist check closes a cancelled train about once a minute, so
    /// the window turned over in under an hour, and a train whose car
    /// was proven after that dropped out of it for good — its landed
    /// branch never inspected again. Paging is what makes the claim
    /// true: the arrived train sits behind 500 cancelled ones here,
    /// which is what the forge looked like on 2026-09-10.
    #[tokio::test]
    async fn the_sweep_reads_the_arrived_train_behind_a_window_of_refusals() {
        let mut all: Vec<Value> = (0..500)
            .map(|i| closed_train(&format!("cancelled-{i}"), false, false))
            .collect();
        all.push(closed_train("arrived-late-proof", true, false));
        // The counterfactual, and the whole defect: the old read took
        // the newest 50 rows, and there is nothing pending in them.
        assert!(
            sweep_pending(&all[..50]).is_empty(),
            "a capped read sees only the refusals — this is what leaked the branch"
        );

        let all_ref = &all;
        let gathered = list_all_pages(|offset| async move {
            let page: Vec<Value> = all_ref
                .iter()
                .skip(offset)
                .take(PAGE_LIMIT)
                .cloned()
                .collect();
            anyhow::Ok(Some(json!({
                "data": page,
                "total": all_ref.len(),
                "offset": offset,
                "limit": PAGE_LIMIT,
            })))
        })
        .await
        .unwrap();

        let pending = sweep_pending(&gathered);
        assert_eq!(
            pending.iter().map(|t| &t["id"]).collect::<Vec<_>>(),
            vec![&json!("arrived-late-proof")],
            "a train proven late must still be swept, however many trains closed since"
        );
    }

    #[test]
    fn a_swept_or_cancelled_train_costs_the_sweep_no_fetches() {
        let trains = vec![
            closed_train("swept", true, true),
            closed_train("cancelled", false, false),
            closed_train("pending", true, false),
        ];
        assert_eq!(
            sweep_pending(&trains)
                .iter()
                .map(|t| &t["id"])
                .collect::<Vec<_>>(),
            vec![&json!("pending")]
        );
    }

    /// A branch a still-open car claims is DEFERRED, not swept — the
    /// claim lifts when that car closes. Stamping the train over it
    /// marks the sweep done while the branch can still become
    /// deletable, and the branch leaks for good.
    #[test]
    fn a_branch_a_live_car_claims_keeps_its_train_pending() {
        let cars = vec![landed_car("car-1", "feat/x")];
        let claimed: BTreeSet<String> = ["feat/x".to_string()].into_iter().collect();
        assert!(
            deletable_branches(&cars, &claimed).is_empty(),
            "the live car's claim beats the landed car's deletion"
        );
        assert_eq!(
            names(&claim_deferred_branches(&cars, &claimed)),
            vec!["feat/x"],
            "and the deferral is named, not silent"
        );
        assert!(
            !sweep_complete(0, claim_deferred_branches(&cars, &claimed).len(), &cars),
            "a deferred branch must not be stamped over"
        );
        // No claim, nothing deferred, every car terminal — done.
        assert!(claim_deferred_branches(&cars, &no_open()).is_empty());
        assert!(sweep_complete(0, 0, &cars));
    }

    #[test]
    fn an_unlanded_car_defers_nothing() {
        let mut open = landed_car("car-1", "feat/x");
        open["status"] = json!("open");
        let mut abandoned = landed_car("car-2", "feat/y");
        abandoned["metadata"]["outcome"] = json!("abandoned");
        let claimed: BTreeSet<String> = ["feat/x".to_string(), "feat/y".to_string()]
            .into_iter()
            .collect();
        // Neither branch's content is on main, so neither was ever the
        // sweep's to delete — nothing to defer, and `main` is never a
        // car's branch to begin with.
        assert!(claim_deferred_branches(&[open, abandoned], &claimed).is_empty());
        let on_main = landed_car("car-3", "main");
        let main_claim: BTreeSet<String> = ["main".to_string()].into_iter().collect();
        assert!(claim_deferred_branches(&[on_main], &main_claim).is_empty());
    }

    #[test]
    fn the_stamp_waits_on_a_failed_branch_a_deferral_and_an_open_car() {
        let landed = landed_car("car-1", "feat/x");
        let mut still_open = landed_car("car-2", "feat/y");
        still_open["status"] = json!("open");
        assert!(sweep_complete(0, 0, std::slice::from_ref(&landed)));
        assert!(!sweep_complete(1, 0, std::slice::from_ref(&landed)));
        assert!(!sweep_complete(0, 1, std::slice::from_ref(&landed)));
        assert!(!sweep_complete(0, 0, &[landed, still_open]));
    }

    // -- the skip reason on the car job ------------------------------------
    //
    // Train #8 conflict-skipped three cars; the journal said "left for
    // the next train" but the car Jobs carried nothing, so the yard's
    // dock showed them unexplained. The PacketCard chip renders
    // `metadata.skip_reason` ("LEFT BEHIND — <reason>"), so the string
    // stays short: a truncated file list, or the missing branch.

    #[test]
    fn a_conflict_skip_reason_names_the_files() {
        let files = vec!["src/a.rs".to_string(), "src/b.rs".to_string()];
        assert_eq!(
            skip_reason_conflict(&files, policy().skip_reason_file_budget),
            "conflict: src/a.rs, src/b.rs"
        );
    }

    #[test]
    fn a_long_conflict_list_truncates_with_a_count() {
        let files: Vec<String> = (0..20)
            .map(|i| format!("crates/core/boss-jobs/src/file_{i:02}.rs"))
            .collect();
        let reason = skip_reason_conflict(&files, policy().skip_reason_file_budget);
        assert!(
            reason.starts_with("conflict: crates/core/boss-jobs/src/file_00.rs"),
            "leads with the first file: {reason}"
        );
        assert!(reason.ends_with("+18 more"), "counts what it hid: {reason}");
        assert!(
            reason.len() <= 120,
            "stays chip-sized ({} chars): {reason}",
            reason.len()
        );
    }

    #[test]
    fn one_huge_conflict_file_is_still_named() {
        // Truncation drops files, never the whole answer: at least one
        // file always shows.
        let long = format!("crates/{}.rs", "x".repeat(150));
        let reason = skip_reason_conflict(
            std::slice::from_ref(&long),
            policy().skip_reason_file_budget,
        );
        assert_eq!(reason, format!("conflict: {long}"));
    }

    #[test]
    fn a_merge_that_died_before_markers_says_so() {
        assert_eq!(
            skip_reason_conflict(&[], policy().skip_reason_file_budget),
            "conflict: unresolved (merge died before conflict markers)"
        );
    }

    /// The dock-depth metric and the boardable count must answer the
    /// same question. On 2026-08-14 they did not: `parked_ready` said
    /// 12 while `candidates` boarded 0, because every branch had been
    /// pushed upstream and none to the fork. The conductor now closes
    /// that gap by copying the ref, so this reason is reserved for a
    /// branch that exists in NEITHER place — a car never pushed at all.
    #[test]
    fn a_branch_missing_everywhere_is_still_a_real_skip() {
        assert_eq!(
            skip_reason_branch_missing("feat/never-pushed"),
            "branch feat/never-pushed not on fork",
            "the skip survives for the genuine case: nothing to copy"
        );
    }

    #[test]
    fn a_missing_branch_skip_reason_names_the_branch() {
        assert_eq!(
            skip_reason_branch_missing("feat/x"),
            "branch feat/x not on fork"
        );
    }

    // -- metadata overlays merge, never clobber ----------------------------
    //
    // jobs-api PUT replaces top-level `metadata` wholesale; every
    // update must carry the existing keys forward, and clearing a key
    // means removing it, not writing "".

    #[test]
    fn a_metadata_overlay_preserves_existing_keys() {
        let job = json!({"metadata": {"branch": "feat/x", "queue": "q-1"}});
        let md = overlay_metadata(&job, vec![("skip_reason", json!("conflict: a.rs"))]);
        assert_eq!(md.get("branch"), Some(&json!("feat/x")));
        assert_eq!(md.get("queue"), Some(&json!("q-1")));
        assert_eq!(md.get("skip_reason"), Some(&json!("conflict: a.rs")));
    }

    #[test]
    fn a_null_overlay_removes_the_key() {
        // Boarding stamps `train` and sheds the stale skip note in one
        // update; the key goes away rather than lingering as "".
        let job = json!({"metadata": {"branch": "feat/x", "skip_reason": "conflict: a.rs"}});
        let md = overlay_metadata(
            &job,
            vec![("train", json!("t-1")), ("skip_reason", Value::Null)],
        );
        assert!(!md.contains_key("skip_reason"));
        assert_eq!(md.get("train"), Some(&json!("t-1")));
        assert_eq!(md.get("branch"), Some(&json!("feat/x")));
    }

    #[test]
    fn an_overlay_on_a_bare_job_starts_fresh() {
        let job = json!({"id": "j-1"});
        let md = overlay_metadata(&job, vec![("skip_reason", json!("x"))]);
        assert_eq!(md.len(), 1);
        // Removing a key that was never there is a quiet no-op.
        let md = overlay_metadata(&job, vec![("skip_reason", Value::Null)]);
        assert!(md.is_empty());
    }

    // -- the drift sentinel (split-brain incident c4b4a6b0) ----------------
    // ----- convergence_verdict — installation is not the finish line
    //
    // fdff316c / 7e5ee013, decided 2026-08-19: the arrival report may
    // only fire once the RUNNING cluster binary self-reports the merge
    // commit, and a lag past the threshold files a packet instead of
    // waiting silently (six unnoticed hours, measured).

    #[test]
    fn commit_identities_match_by_prefix_with_a_floor() {
        let full = "4ee5bba7a17a0123456789abcdef0123456789ab";
        assert!(commits_match("4ee5bba7a17a", full), "short vs full");
        assert!(commits_match(full, "4ee5bba7a17a"), "full vs short");
        assert!(commits_match(full, full), "identical");
        assert!(!commits_match("4ee5bba7a17a", "9da5e4fe1234"), "different");
        // The floor: nothing under 7 chars can match anything — an
        // empty or truncated self-report must never read as converged.
        assert!(!commits_match("", full));
        assert!(!commits_match("4ee5bb", full), "6 chars is below the floor");
    }

    #[test]
    fn a_matching_self_report_converges_regardless_of_elapsed_time() {
        for mins in [0, 29, 500] {
            assert_eq!(
                convergence_verdict(
                    "4ee5bba7a17a",
                    Some("4ee5bba7a17a0123456789ab"),
                    None,
                    mins,
                    30
                ),
                ConvergenceVerdict::Converged,
            );
        }
    }

    #[test]
    fn no_or_wrong_report_waits_inside_the_window_and_alarms_past_it() {
        // None: unreachable, or a binary predating the commit field —
        // absence never converges and times out like any other lag.
        assert_eq!(
            convergence_verdict("4ee5bba7a17a", None, None, 29, 30),
            ConvergenceVerdict::Waiting,
        );
        assert_eq!(
            convergence_verdict("4ee5bba7a17a", None, None, 30, 30),
            ConvergenceVerdict::Overdue,
        );
        // The previous release still running: same shape.
        assert_eq!(
            convergence_verdict("4ee5bba7a17a", Some("d92230071234"), None, 10, 30),
            ConvergenceVerdict::Waiting,
        );
        assert_eq!(
            convergence_verdict("4ee5bba7a17a", Some("d92230071234"), None, 31, 30),
            ConvergenceVerdict::Overdue,
        );
    }
    /// The red-train forensics pin (2026-09-02): a verdict must NAME
    /// what failed. The forge adapter builds this rollup shape and
    /// `ci_check_summary` renders it — the two live apart, so the
    /// contract between them is pinned here (CLAUDE.md 9a). Before the
    /// adapter carried `context`, every red train recorded `?:FAILURE`
    /// and finding the answer cost three calls to the forge API — and
    /// the answer that time was that `test` had died on a disk floor,
    /// not on any code at all.
    #[test]
    fn a_red_check_is_named_in_the_recorded_verdict() {
        let rollup = json!([
            {"context": "build-image", "conclusion": "SUCCESS", "status": "COMPLETED"},
            {"context": "test", "conclusion": "FAILURE", "status": "COMPLETED"},
        ]);
        let summary = ci_check_summary(Some(&rollup));
        assert!(
            summary.contains("test:FAILURE"),
            "the failing check must be named, got: {summary}"
        );
        assert!(!summary.contains("?:"), "no anonymous checks: {summary}");
    }

    /// A blocked deploy tree is quiet inside the window and LOUD past
    /// it — the six-hour silent retry of 2026-09-02, pinned. The first
    /// blocked pass (no stamp yet) is never overdue: elapsed time is
    /// the signal and it has not started elapsing.
    #[test]
    fn a_blocked_deploy_tree_goes_loud_past_the_window() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-09-02T15:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let since = |mins: i64| Some((now - chrono::Duration::minutes(mins)).fixed_offset());
        assert_eq!(
            deploy_block_verdict(None, now, 30),
            DeployBlockVerdict::Waiting,
            "first blocked pass has not started elapsing"
        );
        assert_eq!(
            deploy_block_verdict(since(29), now, 30),
            DeployBlockVerdict::Waiting
        );
        assert_eq!(
            deploy_block_verdict(since(30), now, 30),
            DeployBlockVerdict::Overdue,
            "the boundary is inclusive, like the convergence alarm"
        );
        // The incident's own duration, six hours, must be loud.
        assert_eq!(
            deploy_block_verdict(since(360), now, 30),
            DeployBlockVerdict::Overdue
        );
    }

    /// The rolled-past case (2026-09-02, train #176): the cluster
    /// self-reports a LATER commit that contains this train's merge.
    /// Equality misses; ancestry converges. And git's inability to
    /// answer (None) must never converge — absence of evidence.
    #[test]
    fn a_cluster_rolled_past_the_merge_still_converges_by_ancestry() {
        assert_eq!(
            convergence_verdict("4ee5bba7a17a", Some("d92230071234"), Some(true), 500, 30),
            ConvergenceVerdict::Converged
        );
        assert_eq!(
            convergence_verdict("4ee5bba7a17a", Some("d92230071234"), Some(false), 31, 30),
            ConvergenceVerdict::Overdue
        );
        assert_eq!(
            convergence_verdict("4ee5bba7a17a", None, None, 10, 30),
            ConvergenceVerdict::Waiting
        );
    }

    //
    // BOSS_JOBS_URL defaulted to localhost and the conductor silently
    // booked a whole window on the wrong instance. Preflight goes red
    // on a loopback jobs URL unless the box says it means it.

    #[test]
    fn a_loopback_jobs_url_is_a_preflight_problem() {
        for url in [
            "http://127.0.0.1:7900",
            "http://localhost:7900",
            "http://LOCALHOST:7900",
            "http://[::1]:7900",
            "http://127.9.9.9/api",
        ] {
            let p = local_jobs_problem(url, false)
                .unwrap_or_else(|| panic!("{url} must trip the sentinel"));
            assert!(p.contains("BOSS_JOBS_URL"), "names the env var: {p}");
            assert!(
                p.contains("BOSS_TRAIN_ALLOW_LOCAL_JOBS"),
                "names the override: {p}"
            );
            assert!(
                p.contains("system of record"),
                "names the incident class: {p}"
            );
        }
    }

    #[test]
    fn the_allowance_and_remote_jobs_urls_pass_the_sentinel() {
        // The allowance is the deliberate test/demo-box escape hatch.
        assert!(local_jobs_problem("http://127.0.0.1:7900", true).is_none());
        assert!(local_jobs_problem("http://10.20.0.15:7900", false).is_none());
        assert!(local_jobs_problem("https://jobs.boss.internal/api", false).is_none());
    }

    // -- the arrival report ------------------------------------------------
    //
    // The landing's final structured entry: when the sweep visits an
    // arrived train, it composes what the record proves — the consist,
    // who got left behind, the generation, and the timings the
    // conductor's own `completed_at` stamps make derivable — and files
    // it on the `arrived` step. Missing evidence reads as null, never
    // a guess.

    fn arrived_train() -> serde_json::Value {
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

    fn boarded_cars() -> Vec<serde_json::Value> {
        vec![
            json!({"id": "car-1-uuid-long", "title": "Fix the thing",
                   "metadata": {"branch": "feat/x"}}),
            json!({"id": "car-2-uuid-long", "title": "Add the widget",
                   "metadata": {"branch": "feat/y"}}),
        ]
    }

    /// f402a681: the report moved to the job when terminal steps became
    /// immutable, so the "already filed" check has to read the job too.
    /// Reading the step instead means every reconcile re-files a report
    /// it already wrote.
    #[test]
    fn arrival_filed_is_read_from_the_job_not_the_step() {
        let unfiled = json!({
            "metadata": {"boarded_jobs": []},
            "steps": [{"title": "Train arrived", "status": "completed", "metadata": {}}],
        });
        assert!(!arrival_already_filed(&unfiled));

        let filed = json!({
            "metadata": {"arrival_report": {"consist": []}, "arrival_summary": "2 cars"},
            "steps": [{"title": "Train arrived", "status": "completed", "metadata": {}}],
        });
        assert!(arrival_already_filed(&filed));

        // A report on the STEP is the OLD location. It must not count as
        // filed, or trains that pre-date the move never get a job-level
        // report and their branch sweep stays blocked.
        let old_location = json!({
            "metadata": {},
            "steps": [{
                "title": "Train arrived",
                "status": "completed",
                "metadata": {"arrival_report": {"consist": []}},
            }],
        });
        assert!(
            !arrival_already_filed(&old_location),
            "a report on the step is not a report on the job"
        );
    }

    /// PATCH merges, and a null value DELETES a key — so a null report
    /// must read as "not filed" rather than as a filed one.
    #[test]
    fn a_null_report_is_not_filed() {
        let nulled = json!({"metadata": {"arrival_report": null}});
        assert!(!arrival_already_filed(&nulled));
    }

    #[test]
    fn the_arrival_report_carries_consist_left_behind_and_timings() {
        let report = arrival_report(&arrived_train(), &boarded_cars());
        assert_eq!(
            report["consist"],
            json!([
                {"car_id_short": "car-1-uu", "title": "Fix the thing", "branch": "feat/x"},
                {"car_id_short": "car-2-uu", "title": "Add the widget", "branch": "feat/y"},
            ])
        );
        assert_eq!(
            report["left_behind"],
            json!([{"car_id_short": "car-3-id", "reason": "conflict: src/a.rs"}])
        );
        assert_eq!(report["generation"], json!("abc1234"));
        // merge_ref abc1234def56 IS the deployed generation (short sha
        // prefix) — not distinct, so no merged_sha key.
        assert!(report.get("merged_sha").is_none(), "same commit: {report}");
        assert_eq!(
            report["timings"]["boarded_at"],
            json!("2026-08-13T06:00:00Z")
        );
        assert_eq!(
            report["timings"]["merged_at"],
            json!("2026-08-13T06:05:00Z")
        );
        assert_eq!(
            report["timings"]["deployed_at"],
            json!("2026-08-13T06:12:00Z")
        );
        assert_eq!(
            report["timings"]["arrived_at"],
            json!("2026-08-13T06:20:00Z")
        );
        assert_eq!(report["timings"]["board_to_merge_s"], json!(300));
        assert_eq!(report["timings"]["merge_to_deploy_s"], json!(420));
        assert_eq!(report["timings"]["total_s"], json!(1200));
    }

    #[test]
    fn a_distinct_merge_sha_is_reported() {
        let mut train = arrived_train();
        train["steps"][2]["metadata"]["deployed"] =
            json!("main@999aaaa; 0 applied; services: prod; web: deployed");
        let report = arrival_report(&train, &boarded_cars());
        assert_eq!(report["generation"], json!("999aaaa"));
        assert_eq!(report["merged_sha"], json!("abc1234def56"));
    }

    #[test]
    fn missing_evidence_reads_as_null_never_a_guess() {
        // A train whose steps carry no completed_at stamps (they
        // predate the stamping, or the dispatcher closed `arrived`)
        // and whose deploy summary is absent.
        let train = json!({
            "id": "train-78",
            "status": "closed",
            "metadata": {"boarded_jobs": ["car-1"]},
            "steps": [
                {"spec_slug": "collect", "title": "Collect what is ready to board",
                 "status": "completed", "metadata": {}},
                {"spec_slug": "merged", "title": "Merged into main",
                 "status": "completed", "metadata": {}},
                {"spec_slug": "arrived", "title": "Train arrived",
                 "status": "completed", "metadata": {}},
            ],
        });
        let report = arrival_report(&train, &boarded_cars());
        assert_eq!(report["left_behind"], json!([]));
        assert_eq!(report["generation"], Value::Null);
        // No deployed sha to compare against — the merge evidence is
        // absent too, so no merged_sha key appears.
        assert!(report.get("merged_sha").is_none());
        assert_eq!(report["timings"]["boarded_at"], Value::Null);
        assert_eq!(report["timings"]["arrived_at"], Value::Null);
        assert_eq!(report["timings"]["board_to_merge_s"], Value::Null);
        assert_eq!(report["timings"]["merge_to_deploy_s"], Value::Null);
        assert_eq!(report["timings"]["total_s"], Value::Null);
    }

    #[test]
    fn the_summary_reads_the_report_not_the_world() {
        let full = arrival_report(&arrived_train(), &boarded_cars());
        assert_eq!(
            arrival_summary(&full),
            "2 cars; generation abc1234; total 1200s"
        );
        let bare = arrival_report(&json!({"id": "t", "metadata": {}, "steps": []}), &[]);
        assert_eq!(
            arrival_summary(&bare),
            "0 cars; generation unknown; total ?s"
        );
    }

    // -- the stall sentinel ------------------------------------------------
    //
    // A train counts stalled when open and its newest step completion
    // is older than the threshold. Raising is protocol, cancelling is
    // judgment — the sentinel only makes the stall visible.

    fn ts(s: &str) -> DateTime<Utc> {
        s.parse().unwrap()
    }

    fn train_with_stamps(stamps: &[&str]) -> serde_json::Value {
        let steps: Vec<serde_json::Value> = stamps
            .iter()
            .map(|t| json!({"status": "completed", "metadata": {"completed_at": t}}))
            .collect();
        json!({"id": "t-1", "status": "open", "metadata": {}, "steps": steps})
    }

    #[test]
    fn a_train_past_the_threshold_counts_stalled() {
        let t = train_with_stamps(&["2026-08-13T00:00:00Z"]);
        assert_eq!(stall_age_hours(&t, ts("2026-08-13T08:30:00Z"), 6), Some(8));
        // The boundary counts: exactly at the threshold is stalled.
        assert_eq!(stall_age_hours(&t, ts("2026-08-13T06:00:00Z"), 6), Some(6));
    }

    #[test]
    fn a_train_inside_the_threshold_is_not_stalled() {
        let t = train_with_stamps(&["2026-08-13T00:00:00Z"]);
        assert_eq!(stall_age_hours(&t, ts("2026-08-13T05:59:00Z"), 6), None);
    }

    #[test]
    fn the_newest_completion_is_the_stall_basis() {
        // Unordered stamps: the NEWEST one anchors the age (3h ago),
        // not the oldest (30h ago).
        let t = train_with_stamps(&["2026-08-12T00:00:00Z", "2026-08-13T03:00:00Z"]);
        assert_eq!(stall_age_hours(&t, ts("2026-08-13T06:00:00Z"), 6), None);
        assert_eq!(stall_age_hours(&t, ts("2026-08-13T09:00:00Z"), 6), Some(6));
    }

    #[test]
    fn a_train_without_stamps_never_counts_stalled() {
        // No completion evidence, no basis — the sentinel never
        // guesses an age.
        let t = train_with_stamps(&[]);
        assert_eq!(stall_age_hours(&t, ts("2026-08-13T06:00:00Z"), 6), None);
    }

    // -- auto-cancelling a red train ---------------------------------------
    //
    // The overnight rule: a train that is red AND has stopped moving
    // releases its consist rather than holding it until morning.

    fn red_train(stamps: &[&str], merged: bool) -> serde_json::Value {
        let mut steps: Vec<serde_json::Value> = stamps
            .iter()
            .map(|t| json!({"status": "completed", "metadata": {"completed_at": t}}))
            .collect();
        steps.push(json!({
            "spec_slug": "merged",
            "title": "Merged into main",
            "status": if merged { "completed" } else { "ready" },
            "metadata": {}
        }));
        json!({"id": "t-1", "status": "open", "metadata": {}, "steps": steps})
    }

    #[test]
    fn a_red_train_that_stopped_moving_is_auto_cancelled() {
        let t = red_train(&["2026-08-13T00:00:00Z"], false);
        let r = auto_cancel_reason(&t, "failing", ts("2026-08-13T08:00:00Z"), 6);
        assert!(r.is_some(), "red and 8h stalled should cancel");
        assert!(r.unwrap().contains("8h"), "the reason carries the age");
    }

    #[test]
    fn a_red_train_inside_the_threshold_is_left_alone() {
        // Still young enough that a re-run or a repair may yet save it.
        let t = red_train(&["2026-08-13T00:00:00Z"], false);
        assert_eq!(
            auto_cancel_reason(&t, "failing", ts("2026-08-13T05:00:00Z"), 6),
            None
        );
    }

    #[test]
    fn a_stalled_train_under_repair_is_not_cancelled() {
        // THE REGRESSION THIS EXISTS FOR: a repair has been pushed and
        // CI is re-running, so the LIVE verdict is `pending` even
        // though the train's own `ci` step still reads `failing` from
        // the first run. Deciding from the step would cancel the train
        // the repair was about to save.
        let t = red_train(&["2026-08-13T00:00:00Z"], false);
        assert_eq!(
            auto_cancel_reason(&t, "pending", ts("2026-08-13T09:00:00Z"), 6),
            None
        );
    }

    #[test]
    fn a_green_stalled_train_is_never_auto_cancelled() {
        // Green and stalled means waiting on the merge, not broken —
        // cancelling would throw away a consist that is about to land.
        let t = red_train(&["2026-08-13T00:00:00Z"], false);
        assert_eq!(
            auto_cancel_reason(&t, "green", ts("2026-08-13T09:00:00Z"), 6),
            None
        );
    }

    #[test]
    fn a_merged_train_is_never_auto_cancelled() {
        // The content landed; red post-merge checks are not the
        // consist's problem and its cars must not be released.
        let t = red_train(&["2026-08-13T00:00:00Z"], true);
        assert_eq!(
            auto_cancel_reason(&t, "failing", ts("2026-08-13T09:00:00Z"), 6),
            None
        );
    }

    // -- honouring the operator's cancel request ---------------------------
    //
    // The yard's cancel button (7a24caf3): an operator stamps
    // `cancel_requested` on the train's metadata and reconcile honours
    // it — unstruck, and never on a train that already merged.

    fn requested_train(cancel_requested: serde_json::Value, merged: bool) -> serde_json::Value {
        let mut t = red_train(&[], merged);
        t["metadata"] = json!({ "cancel_requested": cancel_requested });
        t
    }

    #[test]
    fn an_operators_cancel_request_carries_its_reason_and_actor() {
        let t = requested_train(
            json!({"by": "emp-david", "reason": "bad consist", "at": "2026-09-07T01:00:00Z"}),
            false,
        );
        assert_eq!(
            operator_cancel_reason(&t).as_deref(),
            Some("operator cancel: bad consist (by emp-david)")
        );
        assert_eq!(
            operator_cancel_refusal(&t),
            None,
            "an honoured request is not also refused"
        );
    }

    #[test]
    fn no_stamp_is_no_request() {
        assert_eq!(operator_cancel_reason(&red_train(&[], false)), None);
    }

    #[test]
    fn a_request_without_a_reason_or_an_actor_is_not_honoured() {
        // The reason lands on every released car as its skip_reason; a
        // cancel that cannot say why is not one the conductor acts on.
        let t = requested_train(json!({"by": "emp-david", "reason": "  "}), false);
        assert_eq!(operator_cancel_reason(&t), None);
        let t = requested_train(json!({"reason": "bad consist"}), false);
        assert_eq!(operator_cancel_reason(&t), None, "no actor, no request");
    }

    #[test]
    fn a_malformed_stamp_is_not_a_request() {
        let t = requested_train(json!("please cancel"), false);
        assert_eq!(operator_cancel_reason(&t), None);
        assert_eq!(operator_cancel_refusal(&t), None);
    }

    #[test]
    fn a_merged_train_refuses_the_request_once_instead_of_releasing_its_cars() {
        let mut t = requested_train(json!({"by": "emp-david", "reason": "too late"}), true);
        t["steps"][0]["metadata"]["merge_ref"] = json!("abc1234def56");
        assert_eq!(operator_cancel_reason(&t), None, "the content landed");
        assert_eq!(
            operator_cancel_refusal(&t).as_deref(),
            Some("already merged at abc1234def56")
        );
        // Stamped once: a refused train does not re-refuse every pass.
        t["metadata"]["cancel_refused"] = json!("already merged at abc1234def56");
        assert_eq!(operator_cancel_refusal(&t), None);
    }

    // -- a stall is not a red train ----------------------------------------
    //
    // 2026-08-22: two trains stalled through infrastructure incidents.
    // Their runs were cancelled mid-flight, never judging anything, and
    // the conductor read that as red — four innocent cars took a strike
    // each aboard both trains, hit the two-strike hold, and sat through
    // five departures until a human noticed. All four test-merged clean.

    #[test]
    fn a_train_whose_run_was_aborted_still_releases_its_consist() {
        // The release is right — the cars should not be held hostage
        // overnight by a run that will never answer.
        let t = red_train(&["2026-08-13T00:00:00Z"], false);
        let r = auto_cancel_reason(&t, "aborted", ts("2026-08-13T08:00:00Z"), 6)
            .expect("aborted and 8h stalled should release the consist");
        assert!(r.contains("8h"), "the reason carries the age: {r}");
        assert!(
            r.contains("no verdict"),
            "the reason must name the stall, not imply a judgment: {r}"
        );
    }

    #[test]
    fn an_infrastructure_refusal_strikes_no_car() {
        // The locomotive refused before any check ran (train #204,
        // 2026-09-05: 65GB free on the forge, need 70GB) and said so on
        // its commit status. Nothing judged the cars.
        let refused = json!([
            {"context": "CI / build-image (pull_request)", "conclusion": "SUCCESS", "description": ""},
            {"context": "CI / locomotive refusal", "conclusion": "FAILURE",
             "description": "refused: 65GB free on the workspace filesystem, need 70GB"},
        ]);
        assert!(!verdict_strikes_cars("failing", Some(&refused)));
        // A judged red strikes.
        let real_red = json!([
            {"context": "CI / test (pull_request)", "conclusion": "FAILURE", "description": "3 checks failed"},
        ]);
        assert!(verdict_strikes_cars("failing", Some(&real_red)));
        // No description is not a refusal claim.
        let bare = json!([{"context": "CI / test (pull_request)", "conclusion": "FAILURE"}]);
        assert!(
            verdict_strikes_cars("failing", Some(&bare)),
            "no description is not a refusal claim"
        );
        // The word on a PASSING check proves nothing about the failing one.
        let green_mentions = json!([
            {"context": "CI / fast (pull_request)", "conclusion": "SUCCESS", "description": "refused nothing"},
            {"context": "CI / web (pull_request)", "conclusion": "FAILURE", "description": "svelte-check"},
        ]);
        assert!(verdict_strikes_cars("failing", Some(&green_mentions)));
        // Only a failing verdict can strike at all.
        assert!(!verdict_strikes_cars("aborted", Some(&refused)));
    }

    #[test]
    fn an_aborted_train_leaves_its_cars_unstruck() {
        // The strike is what was wrong. Nothing judged these cars.
        assert!(!verdict_strikes_cars("aborted", None));
        let car = json!({"id": "car-1", "metadata": {"red_trains": 1}});
        let stamps = release_stamps(&car, "CI aborted without a verdict", false);
        assert!(
            !stamps.iter().any(|(k, _)| *k == "red_trains"),
            "a stalled train must not touch the strike count"
        );
        // Released all the same: the train marker goes, so it boards again.
        assert_eq!(
            stamps.iter().find(|(k, _)| *k == "train").map(|(_, v)| v),
            Some(&Value::Null)
        );
    }

    #[test]
    fn a_genuinely_red_train_still_strikes_its_cars() {
        // The two-strike hold has to keep working — without it the
        // auto-cancel is a loop that burns CI all night.
        assert!(verdict_strikes_cars("failing", None));
        let car = json!({"id": "car-1", "metadata": {"red_trains": 1}});
        let stamps = release_stamps(&car, "CI red", true);
        assert_eq!(
            stamps
                .iter()
                .find(|(k, _)| *k == "red_trains")
                .map(|(_, v)| v),
            Some(&json!(2)),
            "a red release counts against every car aboard"
        );
    }

    #[test]
    fn only_a_failing_verdict_strikes() {
        // Neither silence nor success is a strike.
        assert!(!verdict_strikes_cars("pending", None));
        assert!(!verdict_strikes_cars("green", None));
    }

    // -- the CI verdict blind spot -----------------------------------------

    #[test]
    fn a_verdict_that_moves_after_recording_is_reported() {
        // The 2026-08-15 case: recorded failing, repaired, red again.
        // Nothing in the system said so for 45 minutes.
        assert!(verdict_drift(Some("failing"), "green").is_some());
        let note = verdict_drift(Some("green"), "failing").expect("green -> failing is a change");
        assert!(note.contains("green"), "the note names where it came from");
        assert!(note.contains("failing"), "and where it went");
    }

    #[test]
    fn an_unchanged_verdict_is_silent() {
        // Reconcile runs every ten minutes; a verdict that has not moved
        // must not produce a line each time or the signal is noise.
        assert_eq!(verdict_drift(Some("failing"), "failing"), None);
        assert_eq!(verdict_drift(Some("green"), "green"), None);
    }

    #[test]
    fn pending_is_not_a_change() {
        // A re-run passes through pending on its way to an answer.
        // Reporting it would fire on every repair, twice.
        assert_eq!(verdict_drift(Some("failing"), "pending"), None);
    }

    #[test]
    fn nothing_recorded_yet_is_not_drift() {
        // Before the step completes, the ordinary path records the
        // first verdict; this is only about the ones after it.
        assert_eq!(verdict_drift(None, "failing"), None);
    }

    #[test]
    fn ci_that_never_answers_is_reported_after_the_threshold() {
        // The case drift cannot see: no verdict at all, so there is
        // nothing to compare against.
        let t = json!({"id":"t-1","status":"open","metadata":{},"steps":[
            {"spec_slug":"pr","title":"Open the batched PR","status":"completed",
             "metadata":{"completed_at":"2026-08-15T06:00:00Z"}},
            {"spec_slug":"ci","title":"CI verdict","status":"ready","metadata":{}}
        ]});
        assert!(ci_overdue(&t, ts("2026-08-15T08:00:00Z"), 2).is_some());
        assert_eq!(ci_overdue(&t, ts("2026-08-15T07:30:00Z"), 2), None);
    }

    #[test]
    fn an_answered_ci_is_never_overdue() {
        // Red counts as answered. A red train is the stall sentinel's
        // problem and auto-cancel's; this signal is only about silence.
        let t = json!({"id":"t-1","status":"open","metadata":{},"steps":[
            {"spec_slug":"pr","title":"Open the batched PR","status":"completed",
             "metadata":{"completed_at":"2026-08-15T06:00:00Z"}},
            {"spec_slug":"ci","title":"CI verdict","status":"completed",
             "metadata":{"result":"failing","completed_at":"2026-08-15T06:20:00Z"}}
        ]});
        assert_eq!(ci_overdue(&t, ts("2026-08-15T20:00:00Z"), 2), None);
    }

    #[test]
    fn a_train_with_no_pr_yet_is_not_overdue() {
        // Nothing has been asked, so nothing is unanswered — a train
        // stuck before its PR belongs to the stall sentinel.
        let t = json!({"id":"t-1","status":"open","metadata":{},"steps":[
            {"spec_slug":"pr","title":"Open the batched PR","status":"ready","metadata":{}},
            {"spec_slug":"ci","title":"CI verdict","status":"pending","metadata":{}}
        ]});
        assert_eq!(ci_overdue(&t, ts("2026-08-16T00:00:00Z"), 2), None);
    }

    // -- the silent decline ------------------------------------------------

    #[test]
    fn a_mergeable_train_the_conductor_declines_to_merge_says_so() {
        // The 2026-09-04 case: green CI, an OPEN PR, and a reconcile run
        // by hand — so BOSS_TRAIN_AUTO_MERGE, which only the conductor's
        // unit sets, was absent. The train was not merged and nothing was
        // logged; two passes read as successful.
        let why = merge_declined_reason(false, "green", Some("OPEN"))
            .expect("green + OPEN + auto-merge off is a decline, not a no-op");
        assert!(
            why.contains("BOSS_TRAIN_AUTO_MERGE"),
            "the reason names the switch that is off: {why}"
        );
        assert!(
            why.contains("unit"),
            "and why a hand-run verb does not have it: {why}"
        );
    }

    #[test]
    fn a_train_the_conductor_does_merge_is_not_a_decline() {
        // The configured conductor merges; the merge itself is the line.
        assert_eq!(merge_declined_reason(true, "green", Some("OPEN")), None);
    }

    #[test]
    fn ordinary_states_are_not_declines() {
        // Reconcile runs every ten minutes over every open train. A train
        // whose CI has not answered, or that is red, or that already
        // landed, is not being declined anything — reporting those here
        // would be a line per train per pass.
        assert_eq!(merge_declined_reason(false, "pending", Some("OPEN")), None);
        assert_eq!(merge_declined_reason(false, "failing", Some("OPEN")), None);
        assert_eq!(merge_declined_reason(false, "green", Some("MERGED")), None);
        assert_eq!(merge_declined_reason(false, "green", Some("CLOSED")), None);
        assert_eq!(merge_declined_reason(false, "green", None), None);
    }

    // -- the two-strike hold -----------------------------------------------

    #[test]
    fn a_car_that_took_two_trains_red_is_held() {
        let car = json!({"id": "car-1", "metadata": {"red_trains": 2}});
        assert!(car_hold_reason(&car, policy().max_red_trains).is_some());
    }

    #[test]
    fn a_car_with_one_red_still_boards() {
        // One red is usually a neighbour's fault — holding on the first
        // would quarantine innocent cars and stall the queue.
        let car = json!({"id": "car-1", "metadata": {"red_trains": 1}});
        assert_eq!(car_hold_reason(&car, policy().max_red_trains), None);
        let fresh = json!({"id": "car-2", "metadata": {}});
        assert_eq!(car_hold_reason(&fresh, policy().max_red_trains), None);
    }

    /// The hold count is DATA now, and this is what that buys: raising
    /// it in the registry lets a car that two reds would have held keep
    /// boarding, with no code change and no train. The decision function
    /// itself never changed — it always took the threshold as an
    /// argument; what changed is where the argument comes from.
    #[test]
    fn the_hold_moves_when_the_policy_says_a_different_number() {
        let lenient = DeliveryPolicy {
            max_red_trains: 3,
            ..policy()
        };
        let two_reds = json!({"id": "car-1", "metadata": {"red_trains": 2}});
        assert_eq!(car_hold_reason(&two_reds, lenient.max_red_trains), None);

        let strict = DeliveryPolicy {
            max_red_trains: 1,
            ..policy()
        };
        let one_red = json!({"id": "car-2", "metadata": {"red_trains": 1}});
        assert!(car_hold_reason(&one_red, strict.max_red_trains).is_some());
    }

    // -- cancelling a train ------------------------------------------------

    #[test]
    fn cancel_releases_only_the_still_open_cars() {
        let open = json!({"id": "car-1", "status": "open",
                          "metadata": {"train": "t-1", "branch": "feat/x"}});
        let landed = landed_car("car-2", "feat/y");
        let mut cancelled = landed_car("car-3", "feat/z");
        cancelled["status"] = json!("cancelled");
        let cars = vec![open, landed, cancelled];
        let released: Vec<&str> = releasable_cars(&cars, "t-1")
            .iter()
            .map(|c| c.get("id").and_then(Value::as_str).unwrap())
            .collect();
        // Closed cars are history — merged or abandoned, not ours to
        // touch. Only the open car returns to the dock.
        assert_eq!(released, vec!["car-1"]);
    }

    /// A CANCEL MUST NOT STRIP A CAR OFF A DIFFERENT, LIVE TRAIN.
    ///
    /// The train's `boarded_jobs` is written once at boarding and never
    /// updated when a car is released, so a long-dead train keeps naming
    /// cars that have since reboarded elsewhere. Cancelling it then
    /// released them again — off a running consist.
    ///
    /// Done on 2026-08-27: cancelling e1de28a3 freed three cars that
    /// were legitimately aboard 1597b4a4, the next board swept them onto
    /// a third train, and two trains believed they carried the same
    /// three cars while the cars named a fourth. The car's own
    /// `metadata.train` is the field `parked_ready` and
    /// `receipt_skip_reason` both read, so it is authoritative; the
    /// train's list is the copy that drifts.
    #[test]
    fn cancel_leaves_a_car_that_has_since_boarded_another_train() {
        let mine = json!({"id": "car-1", "status": "open",
                          "metadata": {"train": "t-1", "branch": "feat/x"}});
        let moved_on = json!({"id": "car-2", "status": "open",
                              "metadata": {"train": "t-2", "branch": "feat/y"}});
        // A car released earlier carries no train at all. It is not ours
        // to re-release, and stamping it again would overwrite a
        // skip_reason that already explains where it has been.
        let already_free = json!({"id": "car-3", "status": "open",
                                  "metadata": {"branch": "feat/z"}});
        let cars = vec![mine, moved_on, already_free];
        let released: Vec<&str> = releasable_cars(&cars, "t-1")
            .iter()
            .map(|c| c.get("id").and_then(Value::as_str).unwrap())
            .collect();
        assert_eq!(
            released,
            vec!["car-1"],
            "only the car whose own metadata.train still names this train may be released"
        );
    }

    #[test]
    fn cancel_deletes_only_the_trains_own_branch_never_a_cars() {
        let train = json!({
            "id": "t-1",
            "subject": {"subject_kind": "custom", "id": "train/20260813-0600"},
        });
        assert_eq!(
            train_branch_to_delete(&train),
            Some("train/20260813-0600".to_string())
        );
        // A subject that is not a train/* branch — whatever went
        // wrong upstream, the cancel path deletes NO car branch.
        let odd = json!({
            "id": "t-2",
            "subject": {"subject_kind": "custom", "id": "feat/x"},
        });
        assert_eq!(train_branch_to_delete(&odd), None);
        assert_eq!(train_branch_to_delete(&json!({"id": "t-3"})), None);
    }

    // -- the arrival branch cleanup ----------------------------------------
    //
    // Cancel has deleted its train's branch since the verb existed;
    // nothing owned the branch after a HAPPY landing, and 62 stale
    // train/* branches accumulated on the forge between 08-13 and
    // 08-20 — squash merges mean ancestry can never classify them
    // after the fact (ab3fa473). The arrival record is the proof, and
    // the cleanup reads it at exactly the right moment.

    /// The forge as a call recorder: `delete_branch` notes the branch
    /// it was asked for and answers as told; every other verb is
    /// unreachable in these tests. The seam the Forge trait exists
    /// for, pointed at the cleanup.
    struct FakeForge {
        deleted: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
        fail_deletes: bool,
    }

    #[async_trait]
    impl Forge for FakeForge {
        async fn pr_info(&self, _url: &str) -> Result<Value> {
            bail!("not exercised")
        }
        async fn pr_create(
            &self,
            _repo: &str,
            _head_branch: &str,
            _title: &str,
            _body: &str,
        ) -> Result<String> {
            bail!("not exercised")
        }
        async fn merge(&self, _url: &str) -> Result<()> {
            bail!("not exercised")
        }
        async fn close_pr(&self, _url: &str) -> Result<()> {
            bail!("not exercised")
        }
        async fn delete_branch(&self, branch: &str) -> Result<bool> {
            self.deleted.lock().unwrap().push(branch.to_string());
            if self.fail_deletes {
                bail!("HTTP 500: forge down");
            }
            Ok(true)
        }
        async fn branch_head(&self, _branch: &str) -> Result<Option<String>> {
            bail!("not exercised")
        }
        async fn cancel_ci_runs(&self, _pr_index: &str, _head_sha: &str) -> Result<usize> {
            bail!("not exercised")
        }
    }

    /// The tree root the config fixtures name.
    ///
    /// `scratch_path` rather than a fixed `/tmp/boss-train-test`, and it
    /// creates nothing — no test here touches the filesystem. The name
    /// still carries the uid and the pid, because a fixed name under the
    /// 1777 shared temp root is the shape that has bitten this repo
    /// fourteen times, and the next test that DOES touch this path would
    /// inherit the collision silently.
    fn train_test_home() -> std::path::PathBuf {
        boss_testing::scratch::scratch_path("boss-train-test")
    }

    /// A conductor whose config is fixtures and whose forge is the
    /// recorder — the cleanup touches neither the jobs API nor the
    /// tree, so nothing else needs to exist.
    fn cleanup_conductor(forge_kind: &str, forge: Box<dyn Forge>) -> Conductor {
        let home = train_test_home();
        Conductor {
            cfg: Config {
                jobs: "http://jobs.invalid".into(),
                gh_repo: "example/boss".into(),
                head_owner: "example".into(),
                fork_url: "https://github.com/example/boss-fork.git".into(),
                upstream_url: "https://github.com/example/boss.git".into(),
                home: home.display().to_string(),
                clone: home.join("repo").display().to_string(),
                deploy_tree: home.join("tree").display().to_string(),
                forge_kind: forge_kind.into(),
                auto_merge: false,
                allow_local_jobs: true,
                ci_hours: 2,
                converge_alarm_mins: 30,
                stranded_alarm_mins: 45,
                auto_park_grace_mins: 10,
                auto_cancel: false,
                ci_host: None,
                dry: false,
            },
            http: reqwest::Client::new(),
            forge,
            policy: policy(),
        }
    }

    /// The `arrived_train` fixture plus the subject the cleanup keys
    /// on — the train's own `train/*` branch.
    fn arrived_train_with_branch() -> Value {
        let mut train = arrived_train();
        train["subject"] = json!({"subject_kind": "custom", "id": "train/20260820-0600"});
        train
    }

    #[tokio::test]
    async fn a_happy_arrival_requests_deletion_of_the_trains_own_branch() {
        let deleted = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let forge = Box::new(FakeForge {
            deleted: std::sync::Arc::clone(&deleted),
            fail_deletes: false,
        });
        let c = cleanup_conductor("forgejo", forge);
        c.clean_arrived_train_branch(&arrived_train_with_branch())
            .await;
        assert_eq!(
            *deleted.lock().unwrap(),
            vec!["train/20260820-0600".to_string()]
        );
    }

    #[tokio::test]
    async fn a_failed_delete_does_not_fail_the_arrival() {
        let deleted = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let forge = Box::new(FakeForge {
            deleted: std::sync::Arc::clone(&deleted),
            fail_deletes: true,
        });
        let c = cleanup_conductor("forgejo", forge);
        // Returns () — there is no Result to fail: the forge blowing
        // up costs a journal line and nothing else. A leftover branch
        // is debt; a failed arrival is an outage.
        c.clean_arrived_train_branch(&arrived_train_with_branch())
            .await;
        // And the delete WAS attempted — the line narrates a real event.
        assert_eq!(
            *deleted.lock().unwrap(),
            vec!["train/20260820-0600".to_string()]
        );
    }

    #[tokio::test]
    async fn only_a_forgejo_happy_arrival_cleans_its_branch() {
        let deleted = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        // Under the github adapter the repo auto-deletes merged
        // train/* PR heads — nothing to own, nothing requested.
        let c = cleanup_conductor(
            "github",
            Box::new(FakeForge {
                deleted: std::sync::Arc::clone(&deleted),
                fail_deletes: false,
            }),
        );
        c.clean_arrived_train_branch(&arrived_train_with_branch())
            .await;
        // A cancelled train closes with `arrived` SKIPPED — its
        // branch was the cancel verb's, deleted at cancel time, and
        // the arrival cleanup asks for nothing.
        let mut cancelled = arrived_train_with_branch();
        cancelled["steps"][3]["status"] = json!("skipped");
        let c2 = cleanup_conductor(
            "forgejo",
            Box::new(FakeForge {
                deleted: std::sync::Arc::clone(&deleted),
                fail_deletes: false,
            }),
        );
        c2.clean_arrived_train_branch(&cancelled).await;
        assert!(deleted.lock().unwrap().is_empty());
        // Cancel's own pin is untouched by the arrival filter: the
        // cancelled train's branch is still exactly the one the
        // cancel path deletes.
        assert_eq!(
            train_branch_to_delete(&cancelled),
            Some("train/20260820-0600".to_string())
        );
    }

    #[test]
    fn the_arrival_cleanup_never_names_a_cars_branch() {
        let mut odd = arrived_train_with_branch();
        odd["subject"] = json!({"subject_kind": "custom", "id": "feat/x"});
        assert_eq!(arrival_branch_to_delete(&odd, "forgejo"), None);
        // The happy case, pure: an arrived record under forgejo names
        // the train's own branch and nothing else.
        assert_eq!(
            arrival_branch_to_delete(&arrived_train_with_branch(), "forgejo"),
            Some("train/20260820-0600".to_string())
        );
    }

    #[test]
    fn the_cleanup_narrates_deletes_and_failures_and_swallows_the_gone() {
        assert_eq!(
            arrival_cleanup_note("train/x", Ok(true)),
            Some("deleted branch train/x (train arrived)".to_string())
        );
        // Already gone says nothing: the sweep revisits an unsettled
        // train every pass, and done work narrated every pass reads
        // as work happening.
        assert_eq!(arrival_cleanup_note("train/x", Ok(false)), None);
        let line = arrival_cleanup_note("train/x", Err(anyhow!("HTTP 500: down")))
            .expect("a failure must be narrated");
        assert!(line.contains("train/x"), "{line}");
        assert!(line.contains("HTTP 500: down"), "{line}");
        assert!(line.contains("arrival stands"), "{line}");
    }

    #[test]
    fn a_cancel_handle_resolves_by_id_prefix_or_pr_url() {
        let a = json!({
            "id": "aaaa1111-2222-3333-4444-555566667777",
            "steps": [{"spec_slug": "pr", "title": "Open the batched PR",
                       "status": "completed",
                       "metadata": {"pr_url": "http://forge/repo/pulls/9"}}],
        });
        let b = json!({"id": "bbbb1111-0000-0000-0000-000000000000", "steps": []});
        let trains = vec![a, b];
        assert_eq!(
            resolve_train(&trains, "aaaa1111-2222-3333-4444-555566667777")
                .unwrap()
                .get("id"),
            trains[0].get("id")
        );
        assert_eq!(
            resolve_train(&trains, "bbbb1111").unwrap().get("id"),
            trains[1].get("id")
        );
        assert_eq!(
            resolve_train(&trains, "http://forge/repo/pulls/9")
                .unwrap()
                .get("id"),
            trains[0].get("id")
        );
        assert!(resolve_train(&trains, "cccc0000").is_err(), "no match");
        // An ambiguous prefix refuses rather than guessing a train.
        let twins = vec![
            json!({"id": "aaaa1111-x", "steps": []}),
            json!({"id": "aaaa1111-y", "steps": []}),
        ];
        assert!(resolve_train(&twins, "aaaa1111").is_err(), "ambiguous");
    }

    // -- the deploy-needed decision ----------------------------------------
    //
    // Live incident: every 10-minute reconcile re-ran a full no-op
    // deploy — generation unchanged, services bounced anyway. The
    // store's `current` key is the 8-char release dirname; ls-remote
    // answers the FULL 40-char sha. full.starts_with(short) is the
    // match — that exact direction, pinned here with the real shapes.

    #[test]
    fn a_generation_already_serving_remote_main_skips_the_deploy() {
        let full = "c0020201aa5f3d9e8b7c6d5e4f3a2b1c0d9e8f7a";
        assert!(
            !deploy_needed("c0020201", full),
            "8-char store key vs 40-char remote sha must read as up to date"
        );
    }

    #[test]
    fn every_other_pair_deploys() {
        let full = "deadbeefaa5f3d9e8b7c6d5e4f3a2b1c0d9e8f7a";
        assert!(deploy_needed("c0020201", full), "different generations");
        // The reversed half-match must never read as up to date.
        assert!(deploy_needed(
            "c0020201aa5f3d9e8b7c6d5e4f3a2b1c0d9e8f7a",
            "c0020201"
        ));
        // Missing evidence on either side deploys — the deploy path
        // surfaces its own errors; a skip must never rest on absence.
        assert!(deploy_needed("", full));
        assert!(deploy_needed("c0020201", ""));
    }

    // -- the playground-deploy-disabled decision ---------------------------
    //
    // The FIRST car of the conductor migration
    // (docs/design/the-cluster-is-the-system.md): move the conductor
    // into the cluster and retire the vestigial boss-gcp playground
    // deploy. A cluster-resident conductor has no `/opt/boss` tree and
    // no sudo, so an empty `deploy_tree` turns the hop OFF — deploy()
    // short-circuits BEFORE any git/tree access and completes the step
    // honestly. The default `/opt/boss` MUST stay enabled so the
    // boss-gcp conductor is byte-unchanged.

    #[test]
    fn an_empty_deploy_tree_disables_the_playground_deploy() {
        // The one intended off-switch: an explicitly-empty tree.
        assert!(playground_deploy_disabled(""));
        // Whitespace-only can only be a mis-set env var, never a path.
        assert!(playground_deploy_disabled("   "));
        assert!(playground_deploy_disabled("\t\n"));
    }

    #[test]
    fn a_real_deploy_tree_keeps_the_playground_deploy() {
        // The default the boss-gcp conductor runs under — unchanged.
        assert!(!playground_deploy_disabled("/opt/boss"));
        // And the scratch path the tree-backed deploy tests exercise.
        assert!(!playground_deploy_disabled(
            &train_test_home().join("tree").display().to_string()
        ));
    }

    #[test]
    fn the_no_playground_deploy_evidence_names_the_convergence_path() {
        // The completion evidence the cluster-resident conductor stamps
        // on the `deployed` step. It reads as a COMPLETION (nothing to
        // deploy), not a block, and points at what actually deploys.
        let ev = NO_PLAYGROUND_DEPLOY_EVIDENCE;
        assert!(ev.contains("no playground deploy"), "states the skip: {ev}");
        assert!(
            ev.contains("converges on forge main"),
            "names where the deploy happens instead: {ev}"
        );
        assert!(
            ev.contains("deploy-runner"),
            "names the actor that deploys: {ev}"
        );
        assert!(
            ev.contains("nothing to deploy from the conductor"),
            "reads as a completion, not a block: {ev}"
        );
    }

    #[test]
    fn repo_path_reads_https_and_ssh_clone_urls() {
        assert_eq!(
            repo_path("https://github.com/dauld/boss-fork.git"),
            "dauld/boss-fork"
        );
        assert_eq!(
            repo_path("git@github.com:dauld/boss-fork"),
            "dauld/boss-fork"
        );
    }

    // -- the sweep's head guard (car 23923b40's known_gap) -----------------
    //
    // `fix/conductor-hardening` boarded at fc55e4d; two more commits
    // (705230b) were pushed to the branch AFTER boarding; the train
    // landed carrying only the boarded ones; the sweep read the job
    // record ("closed, outcome=merged" — true) and deleted the branch,
    // taking the unmerged commits with it. The job record proves the
    // CONTENT landed, never that the branch still holds only that
    // content. These pin the second question the sweep must now ask.

    const BOARDED: &str = "fc55e4d1a2b3c4d5e6f708192a3b4c5d6e7f8091";
    const MOVED: &str = "705230b9f8e7d6c5b4a39281706f5e4d3c2b1a09";

    #[test]
    fn a_branch_still_at_its_boarded_head_is_deleted() {
        assert_eq!(
            sweep_guard(Some(BOARDED), Some(BOARDED)),
            SweepGuard::Delete
        );
    }

    #[test]
    fn a_branch_that_moved_since_boarding_is_kept() {
        // The incident, exactly: the recorded head is not the branch's
        // head any more, so the delete would take work the train never
        // carried.
        assert_eq!(
            sweep_guard(Some(BOARDED), Some(MOVED)),
            SweepGuard::Moved {
                recorded: BOARDED.to_string(),
                current: MOVED.to_string(),
            }
        );
    }

    #[test]
    fn a_car_with_no_recorded_head_keeps_its_branch() {
        // An unknown head is not evidence. A car that boarded before
        // the conductor recorded heads keeps its branch: the cost of
        // keeping one is a stale branch, the cost of deleting one is
        // lost work. The branch has to EXIST for the question to mean
        // anything — see the Gone test for the other half.
        assert_eq!(sweep_guard(None, Some(BOARDED)), SweepGuard::NoRecord);
        // An empty stamp is no stamp.
        assert_eq!(sweep_guard(Some(""), Some(BOARDED)), SweepGuard::NoRecord);
    }

    #[test]
    fn a_branch_already_off_the_forge_is_nothing_to_sweep() {
        assert_eq!(sweep_guard(Some(BOARDED), None), SweepGuard::Gone);
        assert_eq!(sweep_guard(Some(BOARDED), Some("")), SweepGuard::Gone);
        // The forge's answer is asked FIRST, so an absent branch reads
        // Gone whatever the record says. Job 1bd1fb3d: every pre-guard
        // historical car has no recorded head AND no branch left, and
        // ordering the record first made each one a NoRecord line on
        // every reconcile, forever, about a branch swept by hand hours
        // earlier.
        assert_eq!(sweep_guard(None, None), SweepGuard::Gone);
        assert_eq!(sweep_guard(None, Some("")), SweepGuard::Gone);
        assert_eq!(sweep_guard(Some(""), None), SweepGuard::Gone);
    }

    /// One sweep decision, for the tests that are about the line it
    /// earns rather than about which branches were chosen.
    fn decided(branch: &str) -> CarBranch {
        CarBranch {
            branch: branch.to_string(),
            car: "car-1".to_string(),
            head: Some(BOARDED.to_string()),
            rerail_origin: false,
        }
    }

    #[test]
    fn only_a_branch_that_still_exists_is_worth_narrating() {
        // The sweep's journal is an operator surface: a line earns its
        // place by naming something a human can act on. A branch that
        // is not on the forge is not that — nothing to delete, nothing
        // to rescue, no action available.
        assert_eq!(sweep_note(&SweepGuard::Gone, &decided("fix/x")), None);
        // Delete narrates at the call site, which knows whether it was
        // a dry run, a deletion, or a race.
        assert_eq!(sweep_note(&SweepGuard::Delete, &decided("fix/x")), None);
        // The two keep-and-tell cases: the branch exists and the sweep
        // declined it, which is exactly what an operator must hear.
        let no_record = sweep_note(&SweepGuard::NoRecord, &decided("fix/x"))
            .expect("a surviving branch with no record is worth a line");
        assert!(no_record.contains("fix/x"), "{no_record}");
        assert!(
            no_record.contains("no boarded head on record"),
            "{no_record}"
        );
        let moved = sweep_note(
            &SweepGuard::Moved {
                recorded: BOARDED.to_string(),
                current: MOVED.to_string(),
            },
            &decided("fix/conductor-hardening"),
        )
        .expect("a branch that outgrew its boarding is worth a line");
        assert_eq!(
            moved,
            branch_moved_line(&decided("fix/conductor-hardening"), BOARDED, MOVED)
        );
        // A RERAIL ORIGINAL IS NAMED AS ONE, in both refusals. It never
        // boarded anything, so "no boarded head" and "moved since
        // boarding" would send an operator hunting for a boarding that
        // never happened (§Diagnosis — a verdict must name what failed).
        let origin = CarBranch {
            rerail_origin: true,
            ..decided("feat/x")
        };
        let no_head = sweep_note(&SweepGuard::NoRecord, &origin)
            .expect("a surviving original with no recorded head is worth a line");
        assert!(
            no_head.contains("rerail original feat/x") && no_head.contains("no head on record"),
            "{no_head}"
        );
        let origin_moved = sweep_note(
            &SweepGuard::Moved {
                recorded: BOARDED.to_string(),
                current: MOVED.to_string(),
            },
            &origin,
        )
        .expect("an original that moved after the rerail is worth a line");
        assert!(
            origin_moved.contains("rerail original feat/x")
                && origin_moved.contains("moved since the rerail"),
            "{origin_moved}"
        );
    }

    #[test]
    fn the_boarded_head_is_read_off_the_car_job() {
        let mut car = landed_car("car-1", "feat/x");
        car["metadata"]["boarded_head"] = json!(BOARDED);
        assert_eq!(boarded_head(&car), Some(BOARDED));
        // Absent, empty, or non-string reads as no stamp at all.
        assert_eq!(boarded_head(&landed_car("car-2", "feat/y")), None);
        let mut blank = landed_car("car-3", "feat/z");
        blank["metadata"]["boarded_head"] = json!("");
        assert_eq!(boarded_head(&blank), None);
        assert_eq!(boarded_head(&json!({"id": "car-4"})), None);
    }

    #[test]
    fn the_moved_branch_line_names_both_heads() {
        // Operator surface: the only notice that unmerged commits are
        // sitting on a branch the train did not carry.
        assert_eq!(
            branch_moved_line(&decided("fix/conductor-hardening"), BOARDED, MOVED),
            "branch fix/conductor-hardening moved since boarding \
             (fc55e4d1 -> 705230b9) — not deleting"
        );
    }

    // -- the jobs-API retry classifier -------------------------------------
    //
    // The cluster is the system of record and it rolls. Twice on
    // 2026-08-13 a reconcile hit `Connection refused` to the jobs API
    // mid-converge and failed the whole verb; the blip lasted seconds.
    // A bounded retry covers the roll — but only for failures that are
    // blips, and only where re-sending is safe.

    #[test]
    fn a_refused_connection_is_a_blip_under_any_method() {
        // Nothing was received, so nothing was done: even a create may
        // go again.
        assert!(retryable(&Method::GET, &Failure::Connect));
        assert!(retryable(&Method::PUT, &Failure::Connect));
        assert!(retryable(&Method::POST, &Failure::Connect));
    }

    #[test]
    fn an_ambiguous_blip_only_retries_an_idempotent_call() {
        // A timeout leaves the write UNKNOWN — re-POSTing an ambiguous
        // create is how one blip becomes two train Jobs.
        assert!(retryable(&Method::GET, &Failure::Ambiguous));
        assert!(retryable(&Method::PUT, &Failure::Ambiguous));
        assert!(!retryable(&Method::POST, &Failure::Ambiguous));
    }

    #[test]
    fn a_5xx_is_a_blip_and_a_4xx_is_an_answer() {
        for status in [500, 502, 503, 504] {
            assert!(
                retryable(&Method::GET, &Failure::Http(status)),
                "{status} is the SoR failing to answer"
            );
            assert!(
                !retryable(&Method::POST, &Failure::Http(status)),
                "{status} leaves a create ambiguous"
            );
        }
        // A 422 is the jobs API telling the conductor no. Retrying an
        // answer just asks the same question three times — including
        // 429, which is an answer about rate, not a transport blip.
        for status in [400, 404, 409, 422, 429] {
            assert!(!retryable(&Method::GET, &Failure::Http(status)), "{status}");
            assert!(!retryable(&Method::PUT, &Failure::Http(status)), "{status}");
        }
        // 2xx/3xx never reach the classifier, and are not blips either.
        assert!(!retryable(&Method::GET, &Failure::Http(200)));
        assert!(!retryable(&Method::GET, &Failure::Http(301)));
    }

    #[test]
    fn an_unusable_answer_is_never_a_blip() {
        // The SoR answered; the body was garbage. Retrying re-reads
        // the same garbage.
        assert!(!retryable(&Method::GET, &Failure::Malformed));
        assert!(!retryable(&Method::POST, &Failure::Malformed));
    }

    #[test]
    fn the_backoff_doubles_from_the_base() {
        assert_eq!(JOBS_API_RETRY.attempts, 3);
        assert_eq!(JOBS_API_RETRY.backoff(1), Duration::from_secs(2));
        assert_eq!(JOBS_API_RETRY.backoff(2), Duration::from_secs(4));
        // The tests' policy makes the same decisions and never waits.
        assert_eq!(RetryPolicy::immediate(3).backoff(1), Duration::ZERO);
    }

    #[test]
    fn a_blip_cause_reads_the_innermost_error() {
        // "GET /api/jobs: error sending request: ... : Connection
        // refused" — the fact is at the bottom; the url is already
        // implied by the line around it.
        let e = anyhow!("Connection refused (os error 61)")
            .context("error sending request for url (http://10.20.0.34:7900/api/jobs)")
            .context("GET /api/jobs?kind=pr-train");
        assert_eq!(
            short_cause(&e, policy().blip_cause_budget),
            "Connection refused (os error 61)"
        );
        // A bare error is its own innermost cause.
        assert_eq!(
            short_cause(&anyhow!("HTTP 503"), policy().blip_cause_budget),
            "HTTP 503"
        );
        // And it stays journal-sized.
        let long = short_cause(&anyhow!("{}", "x".repeat(500)), policy().blip_cause_budget);
        assert!(long.chars().count() <= 81, "{} chars", long.chars().count());
        assert!(long.ends_with('…'), "says it truncated: {long}");
    }

    #[test]
    fn a_real_refused_connection_classifies_as_a_blip() {
        // The production failure end to end: reqwest's own error for a
        // refused connect must land on a retryable Failure, or the
        // classifier above is pinning a shape the wire never produces.
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let err = rt.block_on(async {
            reqwest::Client::builder()
                .timeout(Duration::from_millis(250))
                .build()
                .unwrap()
                // Port 1 refuses; a filtered port times out. Both are
                // blips, and neither is an answer.
                .get("http://127.0.0.1:1/api/jobs")
                .send()
                .await
                .expect_err("nothing serves port 1")
        });
        let kind = classify_transport(&err);
        assert!(
            matches!(kind, Failure::Connect | Failure::Ambiguous),
            "a refused/timed-out connect must be a transport failure, got {kind:?}"
        );
        assert!(retryable(&Method::GET, &kind));
    }

    // -- the retry driver --------------------------------------------------

    fn blip(kind: Failure) -> ApiFailure {
        ApiFailure {
            kind,
            cause: anyhow!("Connection refused (os error 61)"),
        }
    }

    /// A journal that counts its lines instead of printing them.
    /// Atomic rather than `Cell` because `retrying` now takes a
    /// `Sync` journal (the cadence loop's spawned verb tasks report
    /// through it).
    fn counting_journal(lines: &AtomicU32) -> impl Fn(&str) + Sync {
        move |_| {
            lines.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[tokio::test]
    async fn a_blip_retries_to_the_attempt_budget_then_surfaces() {
        let mut calls = 0u32;
        let lines = AtomicU32::new(0);
        let out: Result<()> = retrying(
            &RetryPolicy::immediate(3),
            &Method::GET,
            policy().blip_cause_budget,
            &counting_journal(&lines),
            || {
                calls += 1;
                async { Err(blip(Failure::Connect)) }
            },
        )
        .await;
        assert!(out.is_err(), "the verb still surfaces a real outage");
        assert_eq!(calls, 3, "three attempts, not more");
        assert_eq!(
            lines.load(Ordering::Relaxed),
            2,
            "one line per retry — blips stay countable"
        );
    }

    #[tokio::test]
    async fn a_recovered_blip_costs_nothing_but_a_line() {
        let mut calls = 0u32;
        let lines = AtomicU32::new(0);
        let out: Result<u8> = retrying(
            &RetryPolicy::immediate(3),
            &Method::PUT,
            policy().blip_cause_budget,
            &counting_journal(&lines),
            || {
                calls += 1;
                let attempt = calls;
                async move {
                    if attempt == 1 {
                        Err(blip(Failure::Ambiguous))
                    } else {
                        Ok(7)
                    }
                }
            },
        )
        .await;
        assert_eq!(out.unwrap(), 7);
        assert_eq!(calls, 2, "stops the moment the SoR answers");
        assert_eq!(lines.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn an_answer_is_surfaced_on_the_first_attempt() {
        let mut calls = 0u32;
        let lines = AtomicU32::new(0);
        let out: Result<()> = retrying(
            &RetryPolicy::immediate(3),
            &Method::PUT,
            policy().blip_cause_budget,
            &counting_journal(&lines),
            || {
                calls += 1;
                async {
                    Err(ApiFailure {
                        kind: Failure::Http(422),
                        cause: anyhow!("PUT /api/jobs/x: HTTP 422: metadata_schema"),
                    })
                }
            },
        )
        .await;
        assert!(
            out.unwrap_err().to_string().contains("422"),
            "the answer reaches the operator unchanged"
        );
        assert_eq!(calls, 1, "a 422 is an answer — asked once");
        assert_eq!(
            lines.load(Ordering::Relaxed),
            0,
            "an answer is not a blip and journals none"
        );
    }

    // ---- publish_car_branch -------------------------------------
    //
    // `candidates` skips any parked car whose branch is not on the
    // fork, and until 2026-08-16 it could recover only one case: the
    // branch already on `origin`. A branch sitting as a LOCAL ref in
    // the conductor's own clone counted as "never pushed at all" —
    // which is exactly what `git push gcp <branch>` produces, since
    // the `gcp` remote IS /var/lib/boss-train/repo. That cost five
    // hand-run pushes in one evening, each a human running
    // `git push origin` from that very directory with credentials the
    // conductor already held.
    //
    // Neither recovery path had a test; only the skip MESSAGE did.
    // These drive real git repositories, because the behaviour is
    // entirely "which refs exist and what does git push do with them"
    // — a faked git would only prove this file agrees with itself.

    /// Removes its directory on drop, so a panicking test does not
    /// leave repositories in /tmp.
    struct Scratch(std::path::PathBuf);
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn git_ok(dir: &std::path::Path, args: &[&str]) {
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
    fn clone_fixture(name: &str) -> (Scratch, std::path::PathBuf) {
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

    fn on_fork(clone: &std::path::Path, branch: &str) -> bool {
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

    fn commit_branch(clone: &std::path::Path, branch: &str) {
        git_ok(clone, &["checkout", "-q", "-b", branch]);
        std::fs::write(clone.join("x"), branch).expect("write");
        git_ok(clone, &["add", "-A"]);
        git_ok(clone, &["commit", "-qm", "work"]);
        git_ok(clone, &["checkout", "-q", "main"]);
    }

    /// One more commit on an existing branch, leaving `main` checked
    /// out — what fixing a car looks like in the conductor's clone.
    fn advance_branch(clone: &std::path::Path, branch: &str, marker: &str) {
        git_ok(clone, &["checkout", "-q", branch]);
        std::fs::write(clone.join("x"), marker).expect("write");
        git_ok(clone, &["add", "-A"]);
        git_ok(clone, &["commit", "-qm", marker]);
        git_ok(clone, &["checkout", "-q", "main"]);
    }

    fn rev(clone: &std::path::Path, refname: &str) -> String {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(clone)
            .args(["rev-parse", refname])
            .output()
            .expect("rev-parse");
        assert!(out.status.success(), "rev-parse {refname}");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// THE BUG THAT REDDENED TWO TRAINS ON 2026-08-17.
    ///
    /// `origin/<branch>` existed and was stale; the fix sat on the
    /// local head. Preferring whichever ref was tried first shipped
    /// the stale commit to the fork, so the train compiled code the
    /// author had already fixed — and reported success doing it.
    #[test]
    fn a_stale_origin_ref_does_not_beat_the_cars_real_head() {
        let (_g, clone) = clone_fixture("stale-origin");
        commit_branch(&clone, "feat/stale");
        git_ok(&clone, &["push", "-q", "origin", "feat/stale"]);
        let stale = rev(&clone, "feat/stale");

        // The fix lands locally and is NOT pushed upstream — exactly
        // what `git push gcp <branch>` leaves behind.
        advance_branch(&clone, "feat/stale", "the fix");
        let real = rev(&clone, "feat/stale");
        assert_ne!(stale, real, "precondition: the branch moved");
        assert_eq!(
            rev(&clone, "origin/feat/stale"),
            stale,
            "precondition: origin is behind"
        );

        assert!(
            publish_car_branch(clone.to_str().expect("utf8"), "feat/stale").expect("publish"),
            "a car with a real head must publish"
        );
        assert_eq!(
            rev(&clone, "fork/feat/stale"),
            real,
            "the fork must carry the car's real head, not the stale origin ref"
        );
    }

    /// The mirror image, and the reason ordering alone is not the fix:
    /// when the fork does not have the branch at all, ANY source
    #[test]
    fn a_fork_that_moved_on_is_never_overwritten() {
        // The car was restacked and force-pushed to the FORK by its author;
        // this clone still holds the pre-restack head locally and as
        // origin/<branch>. Boarding must not publish the stale copy over
        // the fork's newer, diverged head (2026-09-05, packet c1d56d13).
        let (_g, clone) = clone_fixture("fork-moved-on");
        commit_branch(&clone, "feat/moved");
        git_ok(&clone, &["push", "-q", "origin", "feat/moved"]);
        git_ok(&clone, &["push", "-q", "fork", "feat/moved"]);
        let stale = rev(&clone, "feat/moved");
        // A second clone restacks the branch (a new root commit) and
        // force-pushes it to the fork — the author's re-rail.
        let other = clone.parent().expect("root").join("other");
        let fork_url = clone.parent().expect("root").join("fork.git");
        let out = std::process::Command::new("git")
            .args(["clone", "-q", fork_url.to_str().expect("utf8")])
            .arg(&other)
            .output()
            .expect("clone");
        assert!(out.status.success());
        git_ok(&other, &["config", "user.email", "t@example.com"]);
        git_ok(&other, &["config", "user.name", "t"]);
        git_ok(&other, &["checkout", "-q", "--orphan", "feat/moved"]);
        std::fs::write(other.join("README"), "restacked").expect("write");
        git_ok(&other, &["add", "-A"]);
        git_ok(&other, &["commit", "-qm", "restacked"]);
        git_ok(&other, &["push", "-q", "-f", "origin", "feat/moved"]);
        let moved = rev(&other, "feat/moved");
        assert_ne!(moved, stale, "precondition: the fork moved on");

        assert!(
            !publish_car_branch(clone.to_str().expect("utf8"), "feat/moved").expect("publish"),
            "a diverged fork head is not something this clone may overwrite"
        );
        let fork_now = String::from_utf8_lossy(
            &std::process::Command::new("git")
                .args([
                    "-C",
                    fork_url.to_str().expect("utf8"),
                    "rev-parse",
                    "refs/heads/feat/moved",
                ])
                .output()
                .expect("rev-parse")
                .stdout,
        )
        .trim()
        .to_string();
        assert_eq!(
            fork_now, moved,
            "the fork must still carry the author's restacked head"
        );
    }

    /// pushes cleanly, so nothing rejects a stale one. The descendant
    /// has to be chosen deliberately.
    #[test]
    fn an_origin_ref_ahead_of_a_stale_local_head_wins() {
        let (_g, clone) = clone_fixture("stale-local");
        commit_branch(&clone, "feat/ahead");
        // Advance on a scratch clone and push, so `origin/<branch>`
        // moves ahead while this clone's local ref stays put.
        let stale_local = rev(&clone, "feat/ahead");
        git_ok(&clone, &["push", "-q", "origin", "feat/ahead"]);
        advance_branch(&clone, "feat/ahead", "upstream work");
        let ahead = rev(&clone, "feat/ahead");
        git_ok(&clone, &["push", "-q", "origin", "feat/ahead"]);
        git_ok(&clone, &["branch", "-qf", "feat/ahead", &stale_local]);
        assert_eq!(rev(&clone, "feat/ahead"), stale_local, "precondition");

        assert!(publish_car_branch(clone.to_str().expect("utf8"), "feat/ahead").expect("publish"));
        assert_eq!(
            rev(&clone, "fork/feat/ahead"),
            ahead,
            "the newer upstream ref must win over a stale local one"
        );
    }

    // ---- cancellable_run_ids ---------------------------------
    //
    // Shapes copied from a live `GET /actions/runs?limit=N` on the
    // forge on 2026-08-17, INCLUDING `head_branch: null`, which is the
    // whole reason this function exists rather than a one-line filter.

    fn run(id: i64, status: &str, pretty: &str, sha: &str) -> Value {
        json!({
            "id": id,
            "status": status,
            "conclusion": Value::Null,
            "head_branch": Value::Null,
            "prettyref": pretty,
            "commit_sha": sha,
            "event": "pull_request"
        })
    }

    /// The measured trap: keying on `head_branch` finds nothing,
    /// because this forge does not populate it.
    #[test]
    fn a_running_train_job_is_found_without_head_branch() {
        let runs = vec![run(153, "running", "#64", "13c9e4ad")];
        assert_eq!(
            cancellable_run_ids(&runs, "64", "13c9e4ad"),
            vec![153],
            "the run must be identified by prettyref/commit_sha, since head_branch is null"
        );
    }

    #[test]
    fn finished_runs_are_left_alone() {
        let runs = vec![
            run(152, "success", "#64", "13c9e4ad"),
            run(151, "failure", "#64", "13c9e4ad"),
            run(150, "cancelled", "#64", "13c9e4ad"),
        ];
        assert!(
            cancellable_run_ids(&runs, "64", "13c9e4ad").is_empty(),
            "cancelling a finished run is a pointless API call at best"
        );
    }

    /// The expensive mistake this guards against: killing a live run
    /// that belongs to a DIFFERENT train.
    #[test]
    fn another_trains_run_is_never_cancelled() {
        let runs = vec![
            run(153, "running", "#64", "13c9e4ad"),
            run(154, "running", "#65", "deadbeef"),
        ];
        assert_eq!(
            cancellable_run_ids(&runs, "64", "13c9e4ad"),
            vec![153],
            "only this train's runs"
        );
    }

    /// A run queued before the PR existed carries no `#N` but does
    /// carry the sha, so the sha clause has to stand on its own.
    #[test]
    fn a_run_matching_only_on_sha_is_still_ours() {
        let runs = vec![run(155, "running", "", "13c9e4ad")];
        assert_eq!(cancellable_run_ids(&runs, "64", "13c9e4ad"), vec![155]);
    }

    /// Empty selectors must not turn into "match everything" — the
    /// worst possible reading of a cancel.
    #[test]
    fn empty_selectors_cancel_nothing() {
        let runs = vec![run(153, "running", "", "")];
        assert!(cancellable_run_ids(&runs, "", "").is_empty());
    }

    /// An unrecognised status is left running on purpose.
    #[test]
    fn an_unknown_status_is_not_assumed_cancellable() {
        let runs = vec![run(156, "some-future-state", "#64", "13c9e4ad")];
        assert!(
            cancellable_run_ids(&runs, "64", "13c9e4ad").is_empty(),
            "a false cancel costs someone's live run; a miss costs only time"
        );
    }

    /// THE REPAIR PATH, which is where staleness actually costs.
    ///
    /// A car boards, its train reddens, the author fixes the branch and
    /// reboards. `candidates` used to ask only "is this branch on the
    /// fork" — and it is, from the first boarding — so nothing
    /// republished it and the new consist carried the commit that just
    /// failed. Measured twice on 2026-08-17: feat/dev-shared-target was
    /// 3370b42 locally and 96109f7 on the forge, and train 38d49597
    /// assembled the red one.
    #[test]
    fn a_fork_branch_behind_the_car_is_republished() {
        let (_g, clone) = clone_fixture("stale-fork");
        commit_branch(&clone, "feat/repaired");
        let path = clone.to_str().expect("utf8");
        assert!(publish_car_branch(path, "feat/repaired").expect("first publish"));
        let first = rev(&clone, "fork/feat/repaired");

        // The repair.
        advance_branch(&clone, "feat/repaired", "the fix");
        let fixed = rev(&clone, "feat/repaired");
        assert_ne!(first, fixed, "precondition: the branch moved");
        assert_eq!(
            rev(&clone, "fork/feat/repaired"),
            first,
            "precondition: the fork still holds the pre-repair commit"
        );

        assert_eq!(
            car_head(path, "feat/repaired").expect("car_head"),
            Some(fixed.clone()),
            "car_head must report the head the car actually names"
        );
        assert!(publish_car_branch(path, "feat/repaired").expect("republish"));
        assert_eq!(
            rev(&clone, "fork/feat/repaired"),
            fixed,
            "the forge must end up holding the repaired commit"
        );
    }

    /// THE OTHER HALF OF THE REPAIR PATH — a rebase, not a fast-forward.
    ///
    /// The test above covers a car repaired by ADDING a commit, where
    /// the local head is strictly ahead and publishing fast-forwards
    /// the fork onto it. The commoner repair is a REBASE: the author
    /// rebuilds the branch on a newer main and re-pushes it, and the
    /// conductor's clone — which never checked the branch out again —
    /// keeps the pre-rebase commit forever.
    ///
    /// Now the two refs have DIVERGED, so no push can fast-forward and
    /// the fork keeps the commit the gate actually ran on. `car_head`
    /// still reports the local one, and checking a receipt against it
    /// leaves a correctly-gated car behind for "gated, then changed".
    /// Live on 2026-08-29: c6531868, receipt 56b817eb matching the fork
    /// exactly, held out against a local ref eight hours older.
    #[test]
    fn a_rebased_car_boards_the_head_the_fork_holds() {
        let (_g, clone) = clone_fixture("diverged-fork");
        commit_branch(&clone, "fix/rebased");
        let path = clone.to_str().expect("utf8");
        assert!(publish_car_branch(path, "fix/rebased").expect("first publish"));
        let pre_rebase = rev(&clone, "fix/rebased");

        // The author rebases onto a newer base and re-pushes. Both refs
        // now descend from main independently — neither is an ancestor
        // of the other, which is what makes the push unable to help.
        git_ok(&clone, &["checkout", "-q", "-b", "scratch", "main"]);
        std::fs::write(clone.join("x"), "rebased work").expect("write");
        git_ok(&clone, &["add", "-A"]);
        git_ok(&clone, &["commit", "-qm", "rebased work"]);
        git_ok(
            &clone,
            &["push", "-q", "-f", "fork", "scratch:refs/heads/fix/rebased"],
        );
        git_ok(&clone, &["checkout", "-q", "main"]);
        git_ok(&clone, &["fetch", "-q", "fork"]);
        let gated = rev(&clone, "fork/fix/rebased");
        assert_ne!(pre_rebase, gated, "precondition: the refs diverged");
        assert_eq!(
            rev(&clone, "refs/heads/fix/rebased"),
            pre_rebase,
            "precondition: the clone still holds the pre-rebase commit"
        );

        assert_eq!(
            fork_head(path, "fix/rebased").expect("fork_head"),
            Some(gated.clone()),
            "the head that boards is the one the consist is assembled from"
        );

        // The receipt the gate wrote vouches for what is on the fork.
        let car = json!({
            "metadata": {},
            "steps": [{
                "spec_slug": "gate",
                "title": "Green, and observed working",
                "metadata": {"receipt": {"verdict": "green", "head": gated}},
            }],
        });
        assert_eq!(
            receipt_skip_reason(
                &car,
                fork_head(path, "fix/rebased").expect("fork").as_deref()
            ),
            None,
            "a correctly-gated car must board after a rebase"
        );
        let stale =
            receipt_skip_reason(&car, car_head(path, "fix/rebased").expect("car").as_deref())
                .expect("the local ref is the wrong question and must be seen to be");
        assert!(
            stale.contains("gated, then changed"),
            "documents the regression this test pins: {stale}"
        );
    }

    /// A car nobody has pushed anywhere has no head to board.
    #[test]
    fn car_head_is_none_when_the_branch_exists_nowhere() {
        let (_g, clone) = clone_fixture("no-head");
        assert_eq!(
            car_head(clone.to_str().expect("utf8"), "feat/never").expect("car_head"),
            None
        );
    }

    // ---- ci_check_summary ----------------------------------------
    //
    // Shapes taken from a real Forgejo rollup: the adapter builds each
    // entry with `context` and `status`, and `conclusion` is null on
    // this forge (see cancellable_run_ids for the same lesson about
    // trusting field names).

    #[test]
    fn the_ci_summary_names_each_check_and_its_state() {
        let rollup = json!([
            {"context": "CI / fast", "status": "success", "conclusion": Value::Null},
            {"context": "CI / test", "status": "failure", "conclusion": Value::Null},
        ]);
        assert_eq!(
            ci_check_summary(Some(&rollup)),
            "CI / fast:success, CI / test:failure",
            "a red train must say WHICH check, not just that one failed"
        );
    }

    /// The verdict and the detail must agree about the same rollup —
    /// they are two readings of one fetch, and a summary that
    /// disagreed with the verdict would be worse than none.
    #[test]
    fn the_summary_and_the_verdict_read_the_same_rollup() {
        let rollup = json!([
            {"context": "a", "status": "success"},
            {"context": "b", "status": "failure"},
        ]);
        assert_eq!(ci_verdict(Some(&rollup)), "failing");
        assert!(ci_check_summary(Some(&rollup)).contains("b:failure"));
    }

    /// A run that was cancelled judged nothing. Reading it as `failing`
    /// is what struck four innocent cars on 2026-08-22 — the verdict is
    /// the one fact that decides whether a cancel counts against a car,
    /// so it has to distinguish "we looked and it is broken" from "the
    /// run was killed before it could look".
    #[test]
    fn a_cancelled_run_is_not_a_failing_verdict() {
        // Forgejo reports state in `status` with a null `conclusion`.
        let killed = json!([
            {"context": "CI / fast", "status": "success", "conclusion": Value::Null},
            {"context": "CI / test", "status": "cancelled", "conclusion": Value::Null},
        ]);
        assert_eq!(ci_verdict(Some(&killed)), "aborted");
        // A real failure alongside a cancelled sibling is still red —
        // something DID judge the consist and found it wanting.
        let judged = json!([
            {"context": "CI / fast", "status": "failure", "conclusion": Value::Null},
            {"context": "CI / test", "status": "cancelled", "conclusion": Value::Null},
        ]);
        assert_eq!(ci_verdict(Some(&judged)), "failing");
        // A cancelled run alongside one still going has not settled.
        let mid_flight = json!([
            {"context": "CI / fast", "status": "running", "conclusion": Value::Null},
            {"context": "CI / test", "status": "cancelled", "conclusion": Value::Null},
        ]);
        assert_eq!(ci_verdict(Some(&mid_flight)), "pending");
    }

    /// No rollup is not an empty rollup: pending CI has nothing to
    /// report and must not stamp a misleading empty summary as if it
    /// had looked and found nothing.
    #[test]
    fn an_absent_rollup_summarises_to_nothing() {
        assert_eq!(ci_check_summary(None), "");
        assert_eq!(ci_check_summary(Some(&json!([]))), "");
    }

    /// THE CASE THAT COST FIVE PUSHES.
    #[test]
    fn a_branch_local_to_the_clone_gets_published() {
        let (_g, clone) = clone_fixture("local-only");
        commit_branch(&clone, "feat/local-only");
        assert!(!on_fork(&clone, "feat/local-only"), "precondition");

        let published =
            publish_car_branch(clone.to_str().expect("utf8"), "feat/local-only").expect("publish");
        assert!(
            published,
            "a local ref the conductor can see should publish"
        );
        assert!(
            on_fork(&clone, "feat/local-only"),
            "fork/<branch> must resolve afterwards — that is what candidates checks"
        );
    }

    /// The older recovery path, also previously untested.
    #[test]
    fn a_branch_on_origin_gets_copied_to_the_fork() {
        let (_g, clone) = clone_fixture("origin-only");
        commit_branch(&clone, "feat/upstream");
        git_ok(&clone, &["push", "-q", "origin", "feat/upstream"]);
        // Drop the local ref so origin/<branch> is the only source.
        git_ok(&clone, &["branch", "-qD", "feat/upstream"]);

        assert!(!on_fork(&clone, "feat/upstream"));
        assert!(
            publish_car_branch(clone.to_str().expect("utf8"), "feat/upstream").expect("publish")
        );
        assert!(on_fork(&clone, "feat/upstream"));
    }

    /// Recovery must not become "board anything".
    #[test]
    fn a_branch_that_exists_nowhere_is_still_a_skip() {
        let (_g, clone) = clone_fixture("nowhere");
        assert!(
            !publish_car_branch(clone.to_str().expect("utf8"), "feat/never-pushed")
                .expect("publish"),
            "nothing to copy: that car was never pushed, and skipping is right"
        );
        assert!(!on_fork(&clone, "feat/never-pushed"));
    }

    /// `candidates` runs on every boarding attempt, so an unchanged
    /// branch is seen again next train. "Already up to date" must not
    /// read as a failure that strands the car.
    #[test]
    fn publishing_an_already_published_branch_is_idempotent() {
        let (_g, clone) = clone_fixture("twice");
        commit_branch(&clone, "feat/twice");
        let path = clone.to_str().expect("utf8");
        assert!(publish_car_branch(path, "feat/twice").expect("first"));
        assert!(
            publish_car_branch(path, "feat/twice").expect("second"),
            "a second publish of an unchanged branch must still report success"
        );
        assert!(on_fork(&clone, "feat/twice"));
    }

    // ---- the consist check --------------------------------------
    //
    // The combination failures of 2026-08-22..24, reproduced. These
    // drive the REAL lint script out of `infra/lint/`, for the same
    // reason the publish_car_branch fixtures drive real git: the whole
    // claim is "the conductor can answer this question from the
    // assembled tree in seconds", and a faked lint would only prove
    // this file agrees with itself.

    /// Twelve numbered migrations — one over the scrape guard
    /// `migration-numbers-unique.sh` uses to refuse to report on a
    /// directory it clearly failed to read.
    fn twelve_migrations() -> Vec<String> {
        (140..152).map(|n| format!("{n}-thing.sql")).collect()
    }

    /// An assembled tree as the consist check meets it: `infra/lint/`
    /// carrying the real migration-numbers lint, and whatever the cars
    /// dropped into `infra/postgres/schema/`.
    fn consist_fixture(name: &str, migrations: &[String]) -> (Scratch, std::path::PathBuf) {
        let root = std::env::temp_dir().join(format!("boss-consist-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let guard = Scratch(root.clone());
        let lint = root.join("infra/lint");
        let schema = root.join("infra/postgres/schema");
        std::fs::create_dir_all(&lint).expect("mkdir infra/lint");
        std::fs::create_dir_all(&schema).expect("mkdir schema");
        let real = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../infra/lint/migration-numbers-unique.sh");
        std::fs::copy(&real, lint.join("migration-numbers-unique.sh"))
            .unwrap_or_else(|e| panic!("copy {}: {e}", real.display()));
        for m in migrations {
            std::fs::write(schema.join(m), "-- fixture\n").expect("write migration");
        }
        (guard, root)
    }

    #[test]
    fn a_clean_consist_lets_the_train_go_on_to_the_pr() {
        let (_g, tree) = consist_fixture("clean", &twelve_migrations());
        let verdict = consist_check(&tree, &policy());
        assert!(
            matches!(verdict, ConsistVerdict::Proceed { .. }),
            "a tree with no duplicate numbers must not stop a train: {verdict:?}"
        );
        assert_eq!(verdict.ran(), 1, "the one lint in the fixture tree ran");
        assert!(
            verdict.warnings().is_empty(),
            "nothing to warn about: {verdict:?}"
        );
    }

    /// THE FAILURE THAT COST 90 MINUTES OF CI TO LEARN ONE BIT. Two
    /// cars each added `infra/postgres/schema/153-*.sql`. Unique on
    /// each branch, so both passed their own gate; a duplicate the
    /// moment the conductor merged them together.
    #[test]
    fn two_cars_that_both_took_number_153_are_refused_before_the_pr() {
        let mut migrations = twelve_migrations();
        migrations.push("153-dispatcher-rule-cluster-conformance.sql".to_string());
        migrations.push("153-estate-subjects.sql".to_string());
        let (_g, tree) = consist_fixture("dupe-153", &migrations);

        let verdict = consist_check(&tree, &policy());
        let ConsistVerdict::Refuse { failed, .. } = &verdict else {
            panic!("a duplicated migration number must refuse the consist: {verdict:?}");
        };
        assert_eq!(
            failed.len(),
            1,
            "one lint disagreed with this tree: {verdict:?}"
        );
        assert_eq!(
            failed[0].name, "migration-numbers-unique",
            "the refusal names the check, so nobody has to guess"
        );
        // The valuable half: the reason names the lint AND the files,
        // because a combination failure is nobody's car's fault and the
        // cars stay boardable carrying only this string.
        let reason = consist_refusal_reason(failed, policy().skip_reason_file_budget);
        assert!(
            reason.contains("migration-numbers-unique"),
            "reason names the lint: {reason}"
        );
        assert!(
            reason.contains("153-estate-subjects.sql"),
            "reason names a file the lint's own output named: {reason}"
        );
        assert!(
            failed[0]
                .files
                .contains(&"153-dispatcher-rule-cluster-conformance.sql".to_string()),
            "both colliding files are derivable from the output: {:?}",
            failed[0].files
        );
    }

    /// THE COLLISION THAT SURVIVED THE TIMESTAMP (bc7cac00). Two
    /// builders picked `202609082130` in the same minute on
    /// 2026-09-08, so the prefix carries SECONDS now. The lint is the
    /// backstop, and a backstop that only says "you collided" makes
    /// the reader derive the fix: it must name the file to renumber
    /// and the stamp to renumber it to.
    #[test]
    fn the_refusal_names_the_renumber_it_implies() {
        let mut migrations = twelve_migrations();
        migrations.push("20260908213000-sign-off-plugin-v3.sql".to_string());
        migrations.push("20260908213000-arrival-runs-the-probe.sql".to_string());
        let (_g, tree) = consist_fixture("dupe-same-second", &migrations);

        let verdict = consist_check(&tree, &policy());
        let ConsistVerdict::Refuse { failed, .. } = &verdict else {
            panic!("a duplicated timestamp must refuse the consist: {verdict:?}");
        };
        let out = &failed[0].output;
        assert!(
            out.contains("%Y%m%d%H%M%S"),
            "the fix is a fresh SECONDS stamp, and the message must hand over the \
             command that takes one: {out}"
        );
        assert!(
            out.contains("20260908213000"),
            "the message must name the prefix a fresh stamp has to beat, which is \
             the latest one in the tree: {out}"
        );
        assert!(
            out.contains("has NOT been applied"),
            "and which of the two to rename: {out}"
        );
    }

    /// A broken preflight must not become a new way to block every
    /// train. A lint that cannot be run is a logged warning and the
    /// train departs — the check is an accelerant, never a gate.
    #[test]
    fn a_lint_that_cannot_run_warns_and_the_train_still_departs() {
        let (_g, tree) = consist_fixture("ghost", &twelve_migrations());
        // A dangling symlink: the name is in the directory, the script
        // is not on disk. `bash` would exit 127 on it, which must read
        // as "could not run", never as "the tree is bad".
        std::os::unix::fs::symlink(
            tree.join("infra/lint/deleted-by-some-car.sh"),
            tree.join("infra/lint/ghost.sh"),
        )
        .expect("symlink");

        let verdict = consist_check(&tree, &policy());
        assert!(
            matches!(verdict, ConsistVerdict::Proceed { .. }),
            "an unrunnable check must not refuse a consist: {verdict:?}"
        );
        assert!(
            verdict.warnings().iter().any(|w| w.contains("ghost.sh")),
            "and it must say so by name: {:?}",
            verdict.warnings()
        );
        assert_eq!(verdict.ran(), 1, "the runnable lint still ran");
    }

    /// The tamest failure mode of all: a tree with no lints in it.
    #[test]
    fn a_tree_with_no_lint_directory_proceeds_with_a_warning() {
        let root = std::env::temp_dir().join(format!("boss-consist-{}-bare", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let _g = Scratch(root.clone());
        std::fs::create_dir_all(&root).expect("mkdir");
        let verdict = consist_check(&root, &policy());
        assert!(
            matches!(verdict, ConsistVerdict::Proceed { ran: 0, .. }),
            "no lints is not a reason to hold a train: {verdict:?}"
        );
        assert!(!verdict.warnings().is_empty(), "but it is worth a line");
    }

    /// THE FALSE POSITIVE THIS FIXES (2026-09-06). The consist lints
    /// resolve their baseline as `merge-base(origin/main, HEAD)` in the
    /// conductor's clone; a prior train landing leaves that ref stale
    /// until a fetch, and the lint then reads already-landed changes as
    /// this consist's own. `freshen_trunk` points `origin/main` at
    /// CURRENT forge main before the lints run. Simulate a prior train
    /// landing (a second clone advances the forge) while this clone's
    /// `origin/main` lags, then prove one freshen catches it up.
    #[test]
    fn freshen_trunk_catches_origin_main_up_to_the_forge() {
        let (_g, clone) = clone_fixture("freshen-ok");
        let root = clone.parent().expect("root").to_path_buf();
        let origin = root.join("origin.git");

        // Whoever landed the last train, standing in: a second clone
        // advances the forge's main. THIS clone has not fetched since,
        // so its origin/main is now stale.
        let other = root.join("other");
        git_ok(
            &root,
            &[
                "clone",
                "-q",
                origin.to_str().expect("utf8"),
                other.to_str().expect("utf8"),
            ],
        );
        git_ok(&other, &["config", "user.email", "t@example.com"]);
        git_ok(&other, &["config", "user.name", "t"]);
        std::fs::write(other.join("LANDED"), "a prior train").expect("write");
        git_ok(&other, &["add", "-A"]);
        git_ok(&other, &["commit", "-qm", "prior train landed"]);
        git_ok(&other, &["push", "-q", "origin", "main"]);
        let forge_main = rev(&other, "HEAD");

        // Before: the conductor clone's origin/main lags the forge.
        assert_ne!(
            rev(&clone, "origin/main"),
            forge_main,
            "precondition: origin/main is stale"
        );

        freshen_trunk(clone.to_str().expect("utf8"));

        assert_eq!(
            rev(&clone, "origin/main"),
            forge_main,
            "one freshen catches origin/main up to current forge main — which is the ref the \
             consist lints' merge-base baseline is resolved against"
        );
    }

    /// BEST-EFFORT, NON-FATAL. A freshen that cannot reach the remote
    /// must LOG and return, never abort — the consist then proceeds on
    /// the trunk ref as it stands, exactly today's fallback. A missing
    /// `origin` remote makes the fetch exit non-zero; `freshen_trunk`
    /// returning `()` at all is the guarantee (it cannot bail or
    /// panic), and it must leave the tree untouched.
    #[test]
    fn a_failed_freshen_does_not_abort_and_changes_nothing() {
        let root =
            std::env::temp_dir().join(format!("boss-freshen-{}-noremote", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let _g = Scratch(root.clone());
        std::fs::create_dir_all(&root).expect("mkdir");
        git_ok(&root, &["init", "-q", "-b", "main"]);
        git_ok(&root, &["config", "user.email", "t@example.com"]);
        git_ok(&root, &["config", "user.name", "t"]);
        std::fs::write(root.join("README"), "no origin remote here").expect("write");
        git_ok(&root, &["add", "-A"]);
        git_ok(&root, &["commit", "-qm", "base"]);
        let head_before = rev(&root, "HEAD");

        // No `origin` remote: the fetch exits non-zero. freshen_trunk
        // must swallow it and leave the tree exactly as it was.
        freshen_trunk(root.to_str().expect("utf8"));

        assert_eq!(
            rev(&root, "HEAD"),
            head_before,
            "a failed freshen holds no train and changes nothing"
        );
    }

    /// Discovery over the REAL `infra/lint/`, which is the claim that
    /// matters: the roster is the directory, so a lint arriving ON a
    /// train is asked without anyone editing this file. Only the
    /// listing is exercised here — running the set costs ~9 seconds
    /// and its verdict depends on the working tree, neither of which
    /// belongs in a unit test.
    ///
    /// The exclusions are pinned in both directions: each must be out
    /// of the roster AND still be a real script, because an exemption
    /// naming a file that is gone covers nothing and only misleads the
    /// next reader.
    #[test]
    fn the_roster_is_the_lint_directory_itself() {
        let lint_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../infra/lint");
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
        let names: Vec<String> = cheap_lints(&root, &policy())
            .expect("the tree has an infra/lint")
            .iter()
            .map(|p| p.file_name().unwrap_or_default().to_string_lossy().into())
            .collect();
        assert!(
            names.len() > 15,
            "the whole cheap set runs, not a hand-picked pair: {names:?}"
        );
        assert!(
            names.iter().any(|n| n == "migration-numbers-unique.sh"),
            "the lint that catches duplicate migration numbers is in: {names:?}"
        );
        for excluded in &policy().excluded_lints {
            let (script, why) = (&excluded.script, &excluded.reason);
            assert!(
                !names.iter().any(|n| n == script),
                "{script} needs more than a tree ({why}) and must stay out: {names:?}"
            );
            assert!(
                lint_dir.join(script).is_file(),
                "{script} is excluded ({why}) but is no longer in infra/lint/ — drop the \
                 exemption rather than leaving it to mislead"
            );
        }
    }

    /// The exclusion roster is DATA now: a policy that excuses one more
    /// lint excuses it on the next boarding, with no code change and no
    /// train. This is the property the design was bought for, exercised
    /// against the real `infra/lint/` directory.
    #[test]
    fn an_exclusion_added_to_the_policy_takes_a_lint_out_of_the_roster() {
        use crate::delivery_policy::ExcludedLint;
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
        let victim = "migration-numbers-unique.sh";
        let mut excused = policy();
        excused.excluded_lints.push(ExcludedLint {
            script: victim.to_string(),
            reason: "excused by this test, not by the registry".to_string(),
        });
        let names: Vec<String> = cheap_lints(&root, &excused)
            .expect("the tree has an infra/lint")
            .iter()
            .map(|p| p.file_name().unwrap_or_default().to_string_lossy().into())
            .collect();
        // Same floor the sibling test above uses, on purpose: one
        // opinion per derivation. A negative claim — "the victim is not
        // on the roster" — is the vacuity-prone direction, true of an
        // empty roster, and `cheap_lints` returning nothing is exactly
        // the failure this would then hide.
        boss_testing::assert_roster_floor!(
            names,
            15,
            "infra/lint/ minus the policy's exclusions (69 scripts less 4 today)"
        );
        assert!(
            !names.iter().any(|n| n == victim),
            "the roster is the directory MINUS the policy's exclusions: {names:?}"
        );
    }

    #[test]
    fn a_lints_output_gives_up_the_files_it_names() {
        assert_eq!(
            files_named_in(
                "  153:\n    153-a.sql\n    153-b.sql\n",
                policy().consist_files_named
            ),
            vec!["153-a.sql", "153-b.sql"]
        );
        assert_eq!(
            files_named_in(
                "VIOLATION: infra/postgres/schema/100-a.sql was M-changed",
                policy().consist_files_named
            ),
            vec!["infra/postgres/schema/100-a.sql"]
        );
        assert!(
            files_named_in(
                "one-palette: 3 offences found, see above. e.g. below",
                policy().consist_files_named
            )
            .is_empty(),
            "prose is not a file list"
        );
        assert!(
            files_named_in(
                "bumped to v1.2 in Cargo.toml 0.8",
                policy().consist_files_named
            )
            .iter()
            .all(|f| f == "Cargo.toml"),
            "version numbers are not filenames"
        );
    }

    // -- cancel releases cars only after the forge write succeeds -----
    struct CancelForge {
        close_called: std::sync::Arc<std::sync::Mutex<bool>>,
    }
    #[async_trait::async_trait]
    impl Forge for CancelForge {
        async fn pr_info(&self, _url: &str) -> Result<Value> {
            bail!("not exercised")
        }
        async fn pr_create(
            &self,
            _repo: &str,
            _head_branch: &str,
            _title: &str,
            _body: &str,
        ) -> Result<String> {
            bail!("not exercised")
        }
        async fn merge(&self, _url: &str) -> Result<()> {
            bail!("not exercised")
        }
        async fn close_pr(&self, _url: &str) -> Result<()> {
            *self.close_called.lock().unwrap() = true;
            bail!("HTTP 403: forge write refused")
        }
        async fn delete_branch(&self, _branch: &str) -> Result<bool> {
            bail!("not exercised")
        }
        async fn branch_head(&self, _branch: &str) -> Result<Option<String>> {
            bail!("not exercised")
        }
        async fn cancel_ci_runs(&self, _pr_index: &str, _head_sha: &str) -> Result<usize> {
            Ok(0)
        }
    }

    /// 10bb1e1a: releasing a car is a jobs-API metadata write; closing
    /// the PR is the flaky forge write. Releasing FIRST left
    /// "half-cancelled" trains — cars back on the dock, PR still open —
    /// when close_pr's `?` returned Err with the release already done.
    /// This drives cancel_train against a real in-process jobs server
    /// with a forge whose close_pr FAILS, and asserts NO car was
    /// released: the release now happens only after the PR is closed.
    #[tokio::test]
    async fn cancel_does_not_release_cars_when_close_pr_fails() {
        use axum::extract::Path;
        use axum::routing::get;
        use axum::{Json, Router};
        use std::sync::{Arc, Mutex};

        let train = json!({
            "id": "t1", "kind": "pr-train", "status": "open",
            "metadata": { "boarded_jobs": ["c1"], "train_ref": "train/x@abcdef1" },
            "steps": [
                {"id":"s-pr","spec_slug":"pr","title":"Open the batched PR","status":"completed","metadata":{"pr_url":"https://forge.example/david/boss/pulls/9"}},
                {"id":"s-collect","spec_slug":"collect","title":"Collect what is ready to board","status":"completed","metadata":{}},
                {"id":"s-cancelled","spec_slug":"cancelled","title":"Cancelled — nothing to board","status":"ready","metadata":{}}
            ]
        });
        let car = json!({
            "id": "c1", "kind": "ship-a-change", "status": "open",
            "metadata": { "train": "t1", "branch": "fix/x" },
            "steps": [{"id":"c-rev","spec_slug":"review","title":"Open for review","status":"ready","metadata":{}}]
        });

        // Every PUT the conductor makes; a release is a PUT /api/jobs/{car}.
        let puts: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

        let train_list = train.clone();
        let train_one = train.clone();
        let car_one = car.clone();
        let puts_route = puts.clone();
        let app = Router::new()
            .route(
                "/api/jobs",
                get(move || {
                    let train = train_list.clone();
                    async move { Json(json!({ "data": [train] })) }
                }),
            )
            .route(
                "/api/jobs/{id}",
                get(move |Path(id): Path<String>| {
                    let (t, c) = (train_one.clone(), car_one.clone());
                    async move { Json(if id == "t1" { t } else { c }) }
                })
                .put(move |Path(id): Path<String>, _b: Json<Value>| {
                    let puts = puts_route.clone();
                    async move {
                        puts.lock().unwrap().push(id);
                        Json(json!({}))
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let close_called = Arc::new(Mutex::new(false));
        let mut c = cleanup_conductor(
            "forgejo",
            Box::new(CancelForge {
                close_called: close_called.clone(),
            }),
        );
        c.cfg.jobs = format!("http://{addr}");

        let res = c.cancel_train("t1", "forge unreachable", false).await;
        assert!(res.is_err(), "cancel must surface the close_pr failure");
        assert!(
            *close_called.lock().unwrap(),
            "close_pr must have been attempted"
        );
        assert!(
            !puts.lock().unwrap().contains(&"c1".to_string()),
            "the car was released despite close_pr failing — a half-cancelled train"
        );
    }

    // -- the yard's cancel button, honoured non-fatally -------------------

    /// The forge the cancel button meets: `close_pr` answers as told and
    /// records the call; CI cancellation and branch deletion are the
    /// no-ops a cancel tolerates.
    struct OperatorCancelForge {
        close_ok: bool,
        close_called: std::sync::Arc<std::sync::Mutex<bool>>,
    }
    #[async_trait::async_trait]
    impl Forge for OperatorCancelForge {
        async fn pr_info(&self, _url: &str) -> Result<Value> {
            bail!("not exercised")
        }
        async fn pr_create(
            &self,
            _repo: &str,
            _head_branch: &str,
            _title: &str,
            _body: &str,
        ) -> Result<String> {
            bail!("not exercised")
        }
        async fn merge(&self, _url: &str) -> Result<()> {
            bail!("not exercised")
        }
        async fn close_pr(&self, _url: &str) -> Result<()> {
            *self.close_called.lock().unwrap() = true;
            if self.close_ok {
                Ok(())
            } else {
                bail!("HTTP 502: forge unreachable")
            }
        }
        async fn delete_branch(&self, _branch: &str) -> Result<bool> {
            Ok(true)
        }
        async fn branch_head(&self, _branch: &str) -> Result<Option<String>> {
            bail!("not exercised")
        }
        async fn cancel_ci_runs(&self, _pr_index: &str, _head_sha: &str) -> Result<usize> {
            Ok(0)
        }
    }

    /// An open train carrying the operator's stamp and one boarded car
    /// that already took a strike on an earlier consist.
    fn requested_open_train() -> Value {
        json!({
            "id": "t1", "kind": "pr-train", "status": "open",
            "metadata": {
                "boarded_jobs": ["c1"], "train_ref": "train/x@abcdef1",
                "cancel_requested": {"by": "emp-david", "reason": "bad consist", "at": "2026-09-07T01:00:00Z"}
            },
            "steps": [
                {"id":"s-pr","spec_slug":"pr","title":"Open the batched PR","status":"completed","metadata":{"pr_url":"https://forge.example/david/boss/pulls/9"}},
                {"id":"s-collect","spec_slug":"collect","title":"Collect what is ready to board","status":"completed","metadata":{}},
                {"id":"s-cancelled","spec_slug":"cancelled","title":"Cancelled — nothing to board","status":"ready","metadata":{}},
                {"id":"s-merged","spec_slug":"merged","title":"Merged into main","status":"ready","metadata":{}}
            ]
        })
    }
    fn struck_boarded_car() -> Value {
        json!({
            "id": "c1", "kind": "ship-a-change", "status": "open",
            "metadata": { "train": "t1", "branch": "fix/x", "red_trains": 1 },
            "steps": [{"id":"c-rev","spec_slug":"review","title":"Open for review","status":"ready","metadata":{}}]
        })
    }

    type JobPuts = std::sync::Arc<std::sync::Mutex<Vec<(String, Value)>>>;
    type StepPuts = std::sync::Arc<std::sync::Mutex<Vec<(String, String, Value)>>>;

    /// An in-process jobs API holding one train and one car, recording
    /// every write. Serves the open pr-train list and both fetches;
    /// every other list answers empty.
    async fn cancel_request_jobs_api(train: Value, car: Value) -> (String, JobPuts, StepPuts) {
        use axum::extract::{Path, RawQuery};
        use axum::routing::{get, put};
        use axum::{Json, Router};
        use std::sync::{Arc, Mutex};

        let job_puts: JobPuts = Arc::new(Mutex::new(Vec::new()));
        let step_puts: StepPuts = Arc::new(Mutex::new(Vec::new()));
        let (train_list, train_one) = (train.clone(), train);
        let (jp, sp) = (job_puts.clone(), step_puts.clone());
        let app = Router::new()
            .route(
                "/api/jobs",
                get(move |RawQuery(q): RawQuery| {
                    let train = train_list.clone();
                    async move {
                        let open_trains =
                            q.unwrap_or_default().contains("kind=pr-train&status=open");
                        let data: Vec<Value> = if open_trains { vec![train] } else { vec![] };
                        Json(json!({ "data": data }))
                    }
                }),
            )
            .route(
                "/api/jobs/{id}",
                get(move |Path(id): Path<String>| {
                    let (t, c) = (train_one.clone(), car.clone());
                    async move { Json(if id == "t1" { t } else { c }) }
                })
                .put(move |Path(id): Path<String>, Json(body): Json<Value>| {
                    let jp = jp.clone();
                    async move {
                        jp.lock().unwrap().push((id, body));
                        Json(json!({}))
                    }
                }),
            )
            .route(
                "/api/jobs/{id}/steps/{sid}",
                put(
                    move |Path((id, sid)): Path<(String, String)>, Json(body): Json<Value>| {
                        let sp = sp.clone();
                        async move {
                            sp.lock().unwrap().push((id, sid, body));
                            Json(json!({}))
                        }
                    },
                ),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{addr}"), job_puts, step_puts)
    }

    fn cancel_request_conductor(
        jobs: String,
        close_ok: bool,
    ) -> (Conductor, std::sync::Arc<std::sync::Mutex<bool>>) {
        let close_called = std::sync::Arc::new(std::sync::Mutex::new(false));
        let mut c = cleanup_conductor(
            "forgejo",
            Box::new(OperatorCancelForge {
                close_ok,
                close_called: close_called.clone(),
            }),
        );
        c.cfg.jobs = jobs;
        (c, close_called)
    }

    /// The button, honoured: an open train stamped `cancel_requested` is
    /// cancelled the way the operator verb cancels — the car released
    /// UNSTRUCK (`red_trains` untouched) with the operator's reason as
    /// its `skip_reason`, the `cancelled` terminal completed with that
    /// reason — and the request claims the train's pass.
    #[tokio::test]
    async fn a_cancel_request_on_an_open_train_releases_its_cars_unstruck() {
        let (jobs, job_puts, step_puts) =
            cancel_request_jobs_api(requested_open_train(), struck_boarded_car()).await;
        let (c, close_called) = cancel_request_conductor(jobs, true);

        assert!(
            c.honour_cancel_request(&requested_open_train(), "t1", Some("OPEN"))
                .await
        );
        assert!(*close_called.lock().unwrap(), "the PR is closed unmerged");

        let puts = job_puts.lock().unwrap();
        let (_, car) = puts
            .iter()
            .find(|(id, _)| id == "c1")
            .expect("the car was released");
        let md = &car["metadata"];
        assert!(
            md.get("train").is_none(),
            "released: the train stamp is gone"
        );
        assert_eq!(
            md["skip_reason"].as_str().unwrap_or_default(),
            "returned to dock: train cancelled (operator cancel: bad consist (by emp-david))"
        );
        assert_eq!(
            md["red_trains"],
            json!(1),
            "an operator's cancel strikes no car"
        );

        let steps = step_puts.lock().unwrap();
        let (_, _, cancelled) = steps
            .iter()
            .find(|(id, sid, _)| id == "t1" && sid == "s-cancelled")
            .expect("the cancelled terminal was completed");
        assert_eq!(
            cancelled["metadata"]["reason"],
            json!("operator cancel: bad consist (by emp-david)")
        );
    }

    /// The forge refuses the close: the train stays intact — no car
    /// released, no terminal completed — the request still claims the
    /// pass (a train under a cancel request must not go on to merge),
    /// and the method RETURNS, because it cannot fail: the reconcile
    /// loop has nothing to `?` and the other trains continue.
    #[tokio::test]
    async fn a_forge_failure_leaves_the_train_intact_and_the_pass_alive() {
        let (jobs, job_puts, step_puts) =
            cancel_request_jobs_api(requested_open_train(), struck_boarded_car()).await;
        let (c, close_called) = cancel_request_conductor(jobs, false);

        assert!(
            c.honour_cancel_request(&requested_open_train(), "t1", Some("OPEN"))
                .await
        );
        assert!(*close_called.lock().unwrap(), "close_pr was attempted");
        assert!(
            job_puts.lock().unwrap().is_empty(),
            "no car released, nothing stamped — a retry next pass is clean"
        );
        assert!(
            step_puts.lock().unwrap().is_empty(),
            "no terminal completed"
        );
    }

    /// A request that arrives after the merge is refused on the record,
    /// once, and does not claim the pass — the landed train goes on to
    /// deploy and converge.
    #[tokio::test]
    async fn a_cancel_request_on_a_merged_train_is_stamped_refused() {
        let mut train = requested_open_train();
        train["steps"][3]["status"] = json!("completed");
        train["steps"][3]["metadata"]["merge_ref"] = json!("abc1234def56");
        let (jobs, job_puts, step_puts) =
            cancel_request_jobs_api(train.clone(), struck_boarded_car()).await;
        let (c, close_called) = cancel_request_conductor(jobs, true);

        assert!(!c.honour_cancel_request(&train, "t1", Some("MERGED")).await);
        assert!(!*close_called.lock().unwrap(), "nothing closed");
        assert!(
            step_puts.lock().unwrap().is_empty(),
            "no terminal completed"
        );
        let puts = job_puts.lock().unwrap();
        assert_eq!(puts.len(), 1, "one stamp on the train, nothing on the car");
        let (id, body) = &puts[0];
        assert_eq!(id, "t1");
        assert_eq!(
            body["metadata"]["cancel_refused"],
            json!("already merged at abc1234def56")
        );
    }

    /// A three-car train where car 2's close write fails. Cars 1 and 3
    /// must STILL get their review closed and `metadata.merged=true` (the
    /// marker the dispatcher watches to close the car Job); only the one
    /// bad car counts as a failure, and the pass does not abort. This is
    /// the orphan bug: the pre-fix loop used `?` and completed `merged`
    /// first, so one bad car left the rest as open residue forever —
    /// inflating the open-car count and starving boarding.
    #[tokio::test]
    async fn a_bad_car_does_not_orphan_the_rest_of_the_train() {
        use axum::extract::Path;
        use axum::routing::{get, put};
        use axum::{Json, Router};
        use std::sync::{Arc, Mutex};

        fn car(id: &str) -> Value {
            json!({
                "id": id, "kind": "ship-a-change", "status": "open",
                "metadata": { "train": "t1", "branch": format!("fix/{id}") },
                "steps": [{"id": format!("{id}-rev"), "spec_slug": "review",
                           "title": "Open for review", "status": "ready", "metadata": {}}]
            })
        }
        let cars = json!({ "c1": car("c1"), "c2": car("c2"), "c3": car("c3") });

        // (car id, endpoint) of every WRITE the conductor made.
        let writes: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));

        let cars_get = cars.clone();
        let writes_step = writes.clone();
        let writes_meta = writes.clone();
        let app = Router::new()
            .route(
                "/api/jobs/{id}",
                get(move |Path(id): Path<String>| {
                    let cars = cars_get.clone();
                    async move { Json(cars.get(&id).cloned().unwrap_or(Value::Null)) }
                })
                .put(move |Path(id): Path<String>, _b: Json<Value>| {
                    let writes = writes_meta.clone();
                    async move {
                        // Car 2's metadata write is the one the SoR refuses
                        // (422 — an answer, not a blip, so it is not retried).
                        if id == "c2" {
                            return (
                                axum::http::StatusCode::UNPROCESSABLE_ENTITY,
                                Json(json!({"error": "no"})),
                            );
                        }
                        writes.lock().unwrap().push((id, "meta".into()));
                        (axum::http::StatusCode::OK, Json(json!({})))
                    }
                }),
            )
            .route(
                "/api/jobs/{id}/steps/{sid}",
                put(
                    move |Path((id, _sid)): Path<(String, String)>, _b: Json<Value>| {
                        let writes = writes_step.clone();
                        async move {
                            if id == "c2" {
                                return (
                                    axum::http::StatusCode::UNPROCESSABLE_ENTITY,
                                    Json(json!({"error": "no"})),
                                );
                            }
                            writes.lock().unwrap().push((id, "review".into()));
                            (axum::http::StatusCode::OK, Json(json!({})))
                        }
                    },
                ),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let mut c = cleanup_conductor(
            "forgejo",
            Box::new(FakeForge {
                deleted: Arc::new(Mutex::new(Vec::new())),
                fail_deletes: false,
            }),
        );
        c.cfg.jobs = format!("http://{addr}");

        let boarded = vec!["c1".to_string(), "c2".to_string(), "c3".to_string()];
        let failures = c
            .close_boarded_cars("t1", &boarded, "abcdef123456", "https://forge/pulls/9")
            .await;

        assert_eq!(failures, 1, "exactly the one bad car (c2) is a failure");
        let w = writes.lock().unwrap();
        for good in ["c1", "c3"] {
            assert!(
                w.contains(&(good.to_string(), "review".to_string())),
                "car {good} must still have its review closed — a bad car must not orphan it"
            );
            assert!(
                w.contains(&(good.to_string(), "meta".to_string())),
                "car {good} must still get metadata.merged — the dispatcher's close marker"
            );
        }
        assert!(
            !w.contains(&("c2".to_string(), "meta".to_string())),
            "c2's write failed, so its close marker must NOT be recorded"
        );
    }

    /// A partial pass leaves the failed car for the next reconcile, and
    /// the re-run is idempotent for the cars that already closed. Pass 1:
    /// car 2's write fails (the other two close). Pass 2: the server now
    /// reports the already-closed reviews as `completed` and car 2's write
    /// succeeds — so `close_boarded_cars` returns 0, re-closes only car 2,
    /// and issues NO duplicate review write for cars 1 and 3.
    #[tokio::test]
    async fn a_partial_close_retries_the_failed_car_idempotently() {
        use axum::extract::Path;
        use axum::routing::{get, put};
        use axum::{Json, Router};
        use std::collections::HashSet;
        use std::sync::{Arc, Mutex};

        // Reviews the server has seen closed; drives idempotence — a car
        // in here reports `review: completed`, so `complete_step`
        // early-returns and issues no second write.
        let reviewed: Arc<Mutex<HashSet<String>>> = Arc::new(Mutex::new(HashSet::new()));
        // Car 2 heals between passes.
        let heal_c2 = Arc::new(Mutex::new(false));
        // (car id, endpoint) of every WRITE, across both passes.
        let writes: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));

        let reviewed_get = reviewed.clone();
        let reviewed_step = reviewed.clone();
        let heal_step = heal_c2.clone();
        let writes_step = writes.clone();
        let writes_meta = writes.clone();
        let app = Router::new()
            .route(
                "/api/jobs/{id}",
                get(move |Path(id): Path<String>| {
                    let reviewed = reviewed_get.clone();
                    async move {
                        let status = if reviewed.lock().unwrap().contains(&id) {
                            "completed"
                        } else {
                            "ready"
                        };
                        Json(json!({
                            "id": id, "kind": "ship-a-change", "status": "open",
                            "metadata": { "train": "t1", "branch": format!("fix/{id}") },
                            "steps": [{"id": format!("{id}-rev"), "spec_slug": "review",
                                       "title": "Open for review", "status": status, "metadata": {}}]
                        }))
                    }
                })
                .put(move |Path(id): Path<String>, _b: Json<Value>| {
                    let (heal, writes) = (heal_step.clone(), writes_meta.clone());
                    async move {
                        if id == "c2" && !*heal.lock().unwrap() {
                            return (
                                axum::http::StatusCode::UNPROCESSABLE_ENTITY,
                                Json(json!({"error": "no"})),
                            );
                        }
                        writes.lock().unwrap().push((id, "meta".into()));
                        (axum::http::StatusCode::OK, Json(json!({})))
                    }
                }),
            )
            .route(
                "/api/jobs/{id}/steps/{sid}",
                put(
                    move |Path((id, _sid)): Path<(String, String)>, _b: Json<Value>| {
                        let (reviewed, writes) = (reviewed_step.clone(), writes_step.clone());
                        async move {
                            writes.lock().unwrap().push((id.clone(), "review".into()));
                            reviewed.lock().unwrap().insert(id);
                            (axum::http::StatusCode::OK, Json(json!({})))
                        }
                    },
                ),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let mut c = cleanup_conductor(
            "forgejo",
            Box::new(FakeForge {
                deleted: Arc::new(Mutex::new(Vec::new())),
                fail_deletes: false,
            }),
        );
        c.cfg.jobs = format!("http://{addr}");
        let boarded = vec!["c1".to_string(), "c2".to_string(), "c3".to_string()];

        // Pass 1: car 2's metadata write is refused — but c2's review PUT
        // still lands first, so only its `meta` write is missing.
        let pass1 = c
            .close_boarded_cars("t1", &boarded, "abcdef123456", "https://forge/pulls/9")
            .await;
        assert_eq!(pass1, 1, "car 2 fails its metadata write on pass 1");

        // Car 2 heals; retry.
        *heal_c2.lock().unwrap() = true;
        let pass2 = c
            .close_boarded_cars("t1", &boarded, "abcdef123456", "https://forge/pulls/9")
            .await;
        assert_eq!(pass2, 0, "the retry recovers car 2 — nothing left orphaned");

        let w = writes.lock().unwrap();
        let review_writes = |id: &str| {
            w.iter()
                .filter(|(cid, ep)| cid == id && ep == "review")
                .count()
        };
        assert_eq!(
            review_writes("c1"),
            1,
            "car 1's review is written once — the retry must NOT re-close a done step"
        );
        assert_eq!(
            review_writes("c3"),
            1,
            "car 3's review is written once — the retry is idempotent"
        );
        assert!(
            w.contains(&("c2".to_string(), "meta".to_string())),
            "car 2's close marker lands on the retry"
        );
    }

    /// The forge as a call recorder for the WHOLE sweep: `delete_branch`
    /// notes the branch, `branch_head` answers a fixed head (so the
    /// guard reads `Delete`), everything else is unreachable. The seam
    /// the sweep's per-branch isolation is proven through.
    struct SweepForge {
        deleted: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
        head: String,
        /// An honest forge forgets a branch it deleted, so the read-back
        /// after DELETE answers None. `false` models the 2026-09-11
        /// forge: answers the delete, keeps the branch (1096b1a4).
        performs_deletes: bool,
    }
    #[async_trait::async_trait]
    impl Forge for SweepForge {
        async fn pr_info(&self, _url: &str) -> Result<Value> {
            bail!("not exercised")
        }
        async fn pr_create(
            &self,
            _repo: &str,
            _head_branch: &str,
            _title: &str,
            _body: &str,
        ) -> Result<String> {
            bail!("not exercised")
        }
        async fn merge(&self, _url: &str) -> Result<()> {
            bail!("not exercised")
        }
        async fn close_pr(&self, _url: &str) -> Result<()> {
            bail!("not exercised")
        }
        async fn delete_branch(&self, branch: &str) -> Result<bool> {
            self.deleted.lock().unwrap().push(branch.to_string());
            Ok(true)
        }
        async fn branch_head(&self, branch: &str) -> Result<Option<String>> {
            if self.performs_deletes && self.deleted.lock().unwrap().iter().any(|b| b == branch) {
                return Ok(None);
            }
            Ok(Some(self.head.clone()))
        }
        async fn cancel_ci_runs(&self, _pr_index: &str, _head_sha: &str) -> Result<usize> {
            bail!("not exercised")
        }
    }

    /// THE FLEET-LEVEL ISOLATION, end to end. Two arrived trains are
    /// pending a sweep; train A's arrival-report write (a PATCH to the
    /// jobs API) returns 500 every pass — the exact shape of a boarded
    /// car deleted (404), a malformed report, or a forge blip. Before
    /// the fix, the `?` on that write aborted the WHOLE sweep, so every
    /// LATER pending train went unswept and its landed branch
    /// accumulated on the forge (recurring disk debt). The sweep is now
    /// best-effort per train: A is isolated and B is still swept.
    ///
    /// Ordered A-then-B deliberately — A is processed first, so an
    /// abort takes B down with it under the old code. The assertion is
    /// simply that B's branch WAS deleted.
    #[tokio::test]
    async fn one_trains_sweep_failing_does_not_block_the_next_train() {
        use axum::extract::{Path, RawQuery};
        use axum::http::StatusCode;
        use axum::response::IntoResponse;
        use axum::routing::get;
        use axum::{Json, Router};
        use std::sync::{Arc, Mutex};

        const HEAD: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

        // A closed (arrived) train, `arrived` completed, one boarded
        // car — the shape the sweep filters in as pending.
        let train = |tid: &str| {
            json!({
                "id": tid, "kind": "pr-train", "status": "closed",
                "metadata": { "boarded_jobs": [format!("car-{tid}")] },
                "steps": [
                    {"id":"s-arr","spec_slug":"arrived","title":"Train arrived","status":"completed","metadata":{}}
                ]
            })
        };
        // A landed car: closed + merged, its branch and boarded head on
        // record, so `deletable_branches` yields it and the guard reads
        // Delete.
        let car = |tid: &str, branch: &str| {
            json!({
                "id": format!("car-{tid}"), "kind": "ship-a-change", "status": "closed",
                "metadata": { "train": tid, "branch": branch, "outcome": "merged", "boarded_head": HEAD },
                "steps": []
            })
        };

        let train_a = train("tA");
        let train_b = train("tB");
        let car_a = car("tA", "fix/a");
        let car_b = car("tB", "fix/b");

        // A-then-B: the failing train is swept first, so an all-or-
        // nothing abort strands B.
        let closed_list = json!({ "data": [train_a.clone(), train_b.clone()], "total": 2 });

        let by_id: std::collections::HashMap<String, Value> = [
            ("tA".to_string(), train_a),
            ("tB".to_string(), train_b),
            ("car-tA".to_string(), car_a),
            ("car-tB".to_string(), car_b),
        ]
        .into_iter()
        .collect();
        let by_id = Arc::new(by_id);

        let list_route = closed_list.clone();
        let by_id_get = by_id.clone();
        let app = Router::new()
            .route(
                "/api/jobs",
                get(move |RawQuery(q): RawQuery| {
                    let list = list_route.clone();
                    async move {
                        let q = q.unwrap_or_default();
                        // pr-train closed → the pending trains; the open
                        // ship-a-change list (open_car_branches) → none.
                        if q.contains("pr-train") {
                            Json(list)
                        } else {
                            Json(json!({ "data": [], "total": 0 }))
                        }
                    }
                }),
            )
            .route(
                "/api/jobs/{id}",
                get(move |Path(id): Path<String>| {
                    let by_id = by_id_get.clone();
                    async move { Json(by_id.get(&id).cloned().unwrap_or(json!({}))) }
                })
                .put(|Path(_id): Path<String>, _b: Json<Value>| async move { Json(json!({})) }),
            )
            .route(
                "/api/jobs/{id}/metadata",
                axum::routing::patch(|Path(id): Path<String>, _b: Json<Value>| async move {
                    // Train A's arrival report cannot be written — the
                    // persistent per-train failure this test isolates.
                    if id == "tA" {
                        (StatusCode::INTERNAL_SERVER_ERROR, "boom").into_response()
                    } else {
                        Json(json!({})).into_response()
                    }
                }),
            );

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let deleted = Arc::new(Mutex::new(Vec::new()));
        // github forge_kind: the train's OWN branch cleanup is a no-op
        // (the repo auto-deletes merged PR heads), keeping the test on
        // the CAR-branch sweep the isolation guards.
        let mut c = cleanup_conductor(
            "github",
            Box::new(SweepForge {
                deleted: deleted.clone(),
                head: HEAD.to_string(),
                performs_deletes: true,
            }),
        );
        c.cfg.jobs = format!("http://{addr}");

        // The sweep stays green — housekeeping is best-effort — and B's
        // branch is deleted despite A failing.
        c.sweep_landed_branches().await.unwrap();

        let deleted = deleted.lock().unwrap().clone();
        assert!(
            deleted.contains(&"fix/b".to_string()),
            "train B's landed branch went unswept because train A failed first — \
             one bad train stranded the fleet (disk debt): {deleted:?}"
        );
        assert!(
            !deleted.contains(&"fix/a".to_string()),
            "train A aborted before its branch loop, so its branch is untouched \
             this pass and retried next: {deleted:?}"
        );
    }

    /// The forge's answer to DELETE is a claim; the read-back is the
    /// fact. Four combinations, two of which are the 2026-09-11 leak.
    #[test]
    fn a_delete_is_judged_by_the_read_back_not_the_answer() {
        assert_eq!(sweep_delete_verdict(true, None), SweepDelete::Deleted);
        assert_eq!(sweep_delete_verdict(false, None), SweepDelete::AlreadyGone);
        assert_eq!(
            sweep_delete_verdict(true, Some("abc")),
            SweepDelete::StillPresent {
                forge_said: "deleted"
            }
        );
        assert_eq!(
            sweep_delete_verdict(false, Some("abc")),
            SweepDelete::StillPresent {
                forge_said: "already gone"
            }
        );
        assert_eq!(
            sweep_delete_verdict(true, Some("")),
            SweepDelete::Deleted,
            "an empty head is no head"
        );
    }

    /// A forge that ANSWERS the delete and does not perform it: the
    /// 2026-09-11 shape (backlog 1096b1a4) — six landed branches on the
    /// forge the next morning under trains stamped swept. The sweep
    /// must read the branch back, count it a failure, leave the train
    /// UNSTAMPED, and write a `sweep_report` on the train that names
    /// the branch and what the forge said — the evidence that used to
    /// live only in a journal outside anyone's reach.
    #[tokio::test]
    async fn a_delete_the_forge_answered_but_did_not_perform_keeps_the_train_pending() {
        use axum::extract::{Path, RawQuery};
        use axum::routing::get;
        use axum::{Json, Router};
        use std::sync::{Arc, Mutex};

        const HEAD: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        let train = json!({
            "id": "tP", "kind": "pr-train", "status": "closed",
            "metadata": { "boarded_jobs": ["car-tP"] },
            "steps": [
                {"id":"s-arr","spec_slug":"arrived","title":"Train arrived","status":"completed","metadata":{}}
            ]
        });
        let car = json!({
            "id": "car-tP", "kind": "ship-a-change", "status": "closed",
            "metadata": { "train": "tP", "branch": "fix/stays", "outcome": "merged", "boarded_head": HEAD },
            "steps": []
        });
        let closed_list = json!({ "data": [train.clone()], "total": 1 });
        let by_id: std::collections::HashMap<String, Value> =
            [("tP".to_string(), train), ("car-tP".to_string(), car)]
                .into_iter()
                .collect();
        let by_id = Arc::new(by_id);
        let patches: Arc<Mutex<Vec<(String, Value)>>> = Arc::new(Mutex::new(Vec::new()));

        let list_route = closed_list.clone();
        let by_id_get = by_id.clone();
        let patches_w = patches.clone();
        let puts_w = patches.clone();
        let app = Router::new()
            .route(
                "/api/jobs",
                get(move |RawQuery(q): RawQuery| {
                    let list = list_route.clone();
                    async move {
                        if q.unwrap_or_default().contains("pr-train") {
                            Json(list)
                        } else {
                            Json(json!({ "data": [], "total": 0 }))
                        }
                    }
                }),
            )
            .route(
                "/api/jobs/{id}",
                get(move |Path(id): Path<String>| {
                    let by_id = by_id_get.clone();
                    async move { Json(by_id.get(&id).cloned().unwrap_or(json!({}))) }
                })
                // merge_job_metadata writes the WHOLE job back with a PUT;
                // the stamp and the report both arrive through this door.
                .put(move |Path(id): Path<String>, Json(b): Json<Value>| {
                    let puts = puts_w.clone();
                    async move {
                        puts.lock().unwrap().push((id, b));
                        Json(json!({}))
                    }
                }),
            )
            .route(
                "/api/jobs/{id}/metadata",
                axum::routing::patch(move |Path(id): Path<String>, Json(b): Json<Value>| {
                    let patches = patches_w.clone();
                    async move {
                        patches.lock().unwrap().push((id, b));
                        Json(json!({}))
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        // SweepForge answers every DELETE with Ok(true) and every
        // branch_head with the same head — a forge that says "deleted"
        // and keeps the branch.
        let deleted = Arc::new(Mutex::new(Vec::new()));
        let mut c = cleanup_conductor(
            "github",
            Box::new(SweepForge {
                deleted: deleted.clone(),
                head: HEAD.to_string(),
                performs_deletes: false,
            }),
        );
        c.cfg.jobs = format!("http://{addr}");
        c.sweep_landed_branches().await.unwrap();

        assert!(
            deleted.lock().unwrap().contains(&"fix/stays".to_string()),
            "the delete was attempted"
        );
        let patches = patches.lock().unwrap().clone();
        let train_patches: Vec<&Value> = patches
            .iter()
            .filter(|(id, _)| id == "tP")
            .map(|(_, b)| b)
            .collect();
        let stamped = train_patches.iter().any(|b| {
            truthy(
                b.pointer("/metadata/branches_swept")
                    .or_else(|| b.get("branches_swept")),
            )
        });
        assert!(
            !stamped,
            "the train was stamped swept while its branch is still on the forge: {train_patches:?}"
        );
        let report = train_patches
            .iter()
            .find_map(|b| {
                b.pointer("/metadata/sweep_report")
                    .or_else(|| b.get("sweep_report"))
            })
            .expect("the sweep wrote a report on the train even though it did not stamp it");
        let text = report.to_string();
        assert!(
            text.contains("fix/stays")
                && text.contains("still present")
                && text.contains("deleted"),
            "the report names the branch, that it is still present, and what the forge said: {text}"
        );
    }

    /// END TO END, the branch the sweep could never see (packet
    /// 473fda1b, generator 1). One arrived train, one landed car that a
    /// rerail had moved onto `feat/x-rerail` — and the original
    /// `feat/x`, which was never a car of its own. The train deletes
    /// what it MERGED, so before this fix the original survived every
    /// sweep forever; now the car's recorded origin is swept in the
    /// same pass, through the same head guard.
    #[tokio::test]
    async fn the_sweep_deletes_a_landed_rerail_original_alongside_its_twin() {
        use axum::extract::{Path, RawQuery};
        use axum::routing::get;
        use axum::{Json, Router};
        use std::sync::{Arc, Mutex};

        const HEAD: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

        let train = json!({
            "id": "t1", "kind": "pr-train", "status": "closed",
            "metadata": { "boarded_jobs": ["car-1"] },
            "steps": [
                {"id":"s-arr","spec_slug":"arrived","title":"Train arrived","status":"completed","metadata":{}}
            ]
        });
        // The car as `boss rerail` leaves it: riding the rerail branch,
        // with the branch it was moved off and that branch's head on the
        // record.
        let car = json!({
            "id": "car-1", "kind": "ship-a-change", "status": "closed",
            "metadata": {
                "train": "t1", "branch": "feat/x-rerail", "outcome": "merged",
                "boarded_head": HEAD,
                "rerail_origins": [{ "branch": "feat/x", "head": HEAD }]
            },
            "steps": []
        });

        let by_id: std::collections::HashMap<String, Value> = [
            ("t1".to_string(), train.clone()),
            ("car-1".to_string(), car),
        ]
        .into_iter()
        .collect();
        let by_id = Arc::new(by_id);
        let closed_list = json!({ "data": [train], "total": 1 });

        let app = Router::new()
            .route(
                "/api/jobs",
                get(move |RawQuery(q): RawQuery| {
                    let list = closed_list.clone();
                    async move {
                        if q.unwrap_or_default().contains("pr-train") {
                            Json(list)
                        } else {
                            Json(json!({ "data": [], "total": 0 }))
                        }
                    }
                }),
            )
            .route(
                "/api/jobs/{id}",
                get(move |Path(id): Path<String>| {
                    let by_id = by_id.clone();
                    async move { Json(by_id.get(&id).cloned().unwrap_or(json!({}))) }
                })
                .put(|Path(_id): Path<String>, _b: Json<Value>| async move { Json(json!({})) }),
            )
            .route(
                "/api/jobs/{id}/metadata",
                axum::routing::patch(|Path(_id): Path<String>, _b: Json<Value>| async move {
                    Json(json!({}))
                }),
            );

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let deleted = Arc::new(Mutex::new(Vec::new()));
        let mut c = cleanup_conductor(
            "github",
            Box::new(SweepForge {
                deleted: deleted.clone(),
                head: HEAD.to_string(),
                performs_deletes: true,
            }),
        );
        c.cfg.jobs = format!("http://{addr}");
        c.sweep_landed_branches().await.unwrap();

        let deleted = deleted.lock().unwrap().clone();
        assert!(
            deleted.contains(&"feat/x-rerail".to_string()),
            "the branch the train merged must still be swept: {deleted:?}"
        );
        assert!(
            deleted.contains(&"feat/x".to_string()),
            "the rerail original was never a car, so only its record can \
             reach it — leaked forever without this: {deleted:?}"
        );
    }

    /// The journal names a rerail original as one. An operator reading
    /// "deleted branch feat/x" for a branch no car ever carried has to
    /// go re-derive where the deletion came from.
    #[test]
    fn the_journal_calls_a_rerail_original_what_it_is() {
        assert_eq!(sweep_subject(&decided("feat/x")), "branch feat/x");
        assert_eq!(
            sweep_subject(&CarBranch {
                rerail_origin: true,
                ..decided("feat/x")
            }),
            "rerail original feat/x"
        );
        // And the deferral line carries the same subject, so a held
        // original reads as one too.
        let held = claim_deferred_line(&CarBranch {
            rerail_origin: true,
            ..decided("feat/x")
        });
        assert!(
            held.contains("rerail original feat/x") && held.contains("train stays pending"),
            "{held}"
        );
    }

    #[test]
    fn a_failed_train_sweep_names_the_train() {
        let line = sweep_train_failed_line("tA-1234567890", &anyhow!("HTTP 500"));
        assert!(line.contains("tA-12345"), "must name the train: {line}");
        assert!(line.contains("HTTP 500"), "must carry the cause: {line}");
        assert!(
            line.contains("other trains continue"),
            "must say the fleet is not blocked: {line}"
        );
    }

    #[test]
    fn a_failed_branch_sweep_names_the_branch() {
        let line = sweep_branch_failed_line("fix/x", "car-abcdef1234", &anyhow!("forge blip"));
        assert!(line.contains("fix/x"), "must name the branch: {line}");
        assert!(line.contains("car-abcd"), "must name the car: {line}");
        assert!(line.contains("forge blip"), "must carry the cause: {line}");
        assert!(
            line.contains("other branches continue"),
            "must say the train's other branches are not blocked: {line}"
        );
    }
}

#[cfg(test)]
mod track_tests {
    use super::{holds_the_track, track_occupied_by};
    use serde_json::{Value, json};

    /// An open train whose `merged` step sits at `status`.
    fn train(id: &str, title: &str, merged: &str) -> Value {
        json!({
            "id": id,
            "title": title,
            "status": "open",
            "steps": [
                {"spec_slug": "collect", "title": "Collect what is ready to board", "status": "completed"},
                {"spec_slug": "merged", "title": "Merged into main", "status": merged},
                {"spec_slug": "converged", "title": "Cluster converged", "status": "pending"}
            ]
        })
    }

    #[test]
    fn an_open_train_occupies_the_track_and_is_named() {
        let open = vec![train(
            "48b67f2e-9970-456f-817a-d085975b915f",
            "PR train 2026-09-05 07:26",
            "ready",
        )];
        assert_eq!(
            track_occupied_by(&open).as_deref(),
            Some("PR train 2026-09-05 07:26 (48b67f2e)")
        );
    }

    #[test]
    fn no_open_train_means_a_clear_track() {
        // The caller lists status=open only; an arrived or cancelled
        // train is closed and never reaches this list.
        assert_eq!(track_occupied_by(&[]), None);
    }

    /// Twin of boss-jobs `yard::tests::a_merged_train_waiting_to_converge_does_not_hold_the_track`
    /// — the board counts the track by the same rule off typed steps.
    #[test]
    fn a_merged_train_waiting_to_converge_does_not_hold_the_track() {
        // 2026-09-07 (f3796323), twice: a merged train whose sha bricked
        // its boot sat at `converged`, and the fix-forward car could not
        // board because the track was "occupied" by the very train it
        // would have converged. Its content is on main; the next consist
        // merges on top and converges it by ancestry.
        let mut merged = train(
            "a1b2c3d4-0000-0000-0000-000000000000",
            "PR train 2026-09-07 21:00",
            "completed",
        );
        merged["steps"][2]["status"] = json!("ready");
        assert!(!holds_the_track(&merged));
        assert_eq!(track_occupied_by(&[merged]), None);
    }

    /// Twin of boss-jobs `yard::tests::only_the_pre_merge_train_counts_as_the_track`.
    #[test]
    fn the_pre_merge_train_is_named_when_a_merged_one_is_also_open() {
        let merged = train(
            "a1b2c3d4-0000-0000-0000-000000000000",
            "PR train 2026-09-07 21:00",
            "completed",
        );
        let pre_merge = train(
            "e5f6a7b8-0000-0000-0000-000000000000",
            "PR train 2026-09-07 22:51",
            "ready",
        );
        assert_eq!(
            track_occupied_by(&[merged, pre_merge]).as_deref(),
            Some("PR train 2026-09-07 22:51 (e5f6a7b8)")
        );
    }

    #[test]
    fn a_train_with_no_steps_fails_closed_as_pre_merge() {
        // Unknown is not "clear": a row the list did not enrich, or a
        // train with no merged step at all, holds the track.
        let bare = json!({
            "id": "48b67f2e-9970-456f-817a-d085975b915f",
            "title": "PR train 2026-09-05 07:26",
            "status": "open"
        });
        assert!(holds_the_track(&bare));
        assert_eq!(
            track_occupied_by(&[bare]).as_deref(),
            Some("PR train 2026-09-05 07:26 (48b67f2e)")
        );
        let no_merged = json!({
            "id": "48b67f2e-9970-456f-817a-d085975b915f",
            "title": "PR train 2026-09-05 07:26",
            "steps": [{"spec_slug": "collect", "title": "Collect what is ready to board", "status": "ready"}]
        });
        assert!(holds_the_track(&no_merged));
    }
}

#[cfg(test)]
mod burial_tests {
    use super::verdict_to_bury;
    use chrono::{TimeZone, Utc};
    use serde_json::json;

    fn run(
        status: &str,
        verdict: &str,
        opened: &str,
        extra: serde_json::Value,
    ) -> serde_json::Value {
        let mut md = json!({"branch": "fix/lean-ci-builds", "sha": "6c0e31a34a7659aebd63c07218cfddc1e3542fb9", "opened_at": opened});
        if let Some(o) = extra.as_object() {
            for (k, v) in o {
                md[k] = v.clone();
            }
        }
        json!({
            "id": "335d5f6d-0000-0000-0000-000000000000", "kind": "gate-run", "status": status, "metadata": md,
            "steps": [{"title": "Record the gate verdict", "spec_slug": "record-verdict", "status": "completed", "metadata": {"verdict": verdict}}]
        })
    }

    #[test]
    fn a_fresh_lost_or_failed_verdict_with_a_sha_is_a_candidate() {
        let now = Utc.with_ymd_and_hms(2026, 9, 5, 17, 0, 0).unwrap();
        for v in ["lost", "failed"] {
            let r = run("closed", v, "2026-09-04T00:20:43Z", json!({}));
            assert_eq!(
                verdict_to_bury(&r, now),
                Some((
                    "6c0e31a34a7659aebd63c07218cfddc1e3542fb9".to_string(),
                    v.to_string()
                ))
            );
        }
    }

    #[test]
    fn a_green_an_open_run_an_annotated_run_and_archaeology_are_left_alone() {
        let now = Utc.with_ymd_and_hms(2026, 9, 5, 17, 0, 0).unwrap();
        assert_eq!(
            verdict_to_bury(
                &run("closed", "green", "2026-09-04T00:20:43Z", json!({})),
                now
            ),
            None
        );
        assert_eq!(
            verdict_to_bury(&run("open", "lost", "2026-09-04T00:20:43Z", json!({})), now),
            None
        );
        assert_eq!(
            verdict_to_bury(
                &run(
                    "closed",
                    "lost",
                    "2026-09-04T00:20:43Z",
                    json!({"superseded": "by hand"})
                ),
                now
            ),
            None
        );
        assert_eq!(
            verdict_to_bury(
                &run("closed", "lost", "2026-09-01T00:20:43Z", json!({})),
                now
            ),
            None,
            "older than the approach window"
        );
        assert_eq!(
            verdict_to_bury(
                &run("closed", "lost", "2026-09-04T00:20:43Z", json!({"sha": ""})),
                now
            ),
            None,
            "no sha, nothing to ask git"
        );
    }
}

#[cfg(test)]
mod stranded_green_tests {
    use super::{
        StrandCause, StrandWindows, StrandedGreen, stranded_alarm_body, stranded_alarms_to_clear,
        stranded_clear_reason, stranded_clear_step_body, stranded_greens_to_alarm,
        stranded_refresh_patch,
    };
    use chrono::{TimeZone, Utc};
    use serde_json::{Map, json};
    use std::collections::BTreeSet;

    /// The windows the live conductor runs on: 45 min for a hand-gated
    /// green, 10 min of grace for auto-park.
    fn windows() -> StrandWindows {
        StrandWindows {
            never_parked_mins: 45,
            auto_park_grace_mins: 10,
        }
    }

    /// A green gate-run for `branch`, opened `opened` (RFC3339), with an
    /// optional verdict-step `completed_at` and optional extra metadata
    /// (e.g. `superseded`, `park_summary`). Mirrors a real gate-run: the
    /// verdict lives on the `record-verdict` step, and `completed_at` is
    /// the STEP'S OWN column, which the jobs API stamps on every
    /// completion — the same place a live packet carries it.
    fn green_run(
        id: &str,
        branch: &str,
        opened: &str,
        completed_at: Option<&str>,
        extra: serde_json::Value,
    ) -> serde_json::Value {
        let mut md = json!({"branch": branch, "opened_at": opened});
        if let Some(o) = extra.as_object() {
            for (k, v) in o {
                md[k] = v.clone();
            }
        }
        let mut step = json!({
            "title": "Record the gate verdict", "spec_slug": "record-verdict",
            "status": "completed", "metadata": {"verdict": "green"}
        });
        if let Some(c) = completed_at {
            step["completed_at"] = json!(c);
        }
        json!({
            "id": id, "kind": "gate-run", "status": "closed", "metadata": md,
            "steps": [step]
        })
    }

    /// The same run with a `--park-*` intent stamped: the shape
    /// `boss gate --park-summary …` leaves, and the flag
    /// `jobs.auto-park` keys on.
    fn green_run_with_intent(
        id: &str,
        branch: &str,
        opened: &str,
        completed_at: Option<&str>,
    ) -> serde_json::Value {
        green_run(
            id,
            branch,
            opened,
            completed_at,
            json!({"park_summary": "does the thing"}),
        )
    }

    fn branches(v: &[&str]) -> BTreeSet<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    /// The reason this alarm exists: a hand-gated green with no car,
    /// aged past its window, no open alarm — selected, carrying its
    /// branch, gate-run id, verdict age and CAUSE.
    #[test]
    fn a_stranded_green_past_threshold_is_selected() {
        let now = Utc.with_ymd_and_hms(2026, 9, 7, 12, 0, 0).unwrap();
        // Went green 90 min ago.
        let runs = [green_run(
            "gr-1",
            "fix/stranded",
            "2026-09-07T10:00:00Z",
            Some("2026-09-07T10:30:00Z"),
            json!({}),
        )];
        let got = stranded_greens_to_alarm(&runs, &branches(&[]), &branches(&[]), now, windows());
        assert_eq!(
            got,
            vec![StrandedGreen {
                branch: "fix/stranded".into(),
                gate_run_id: "gr-1".into(),
                age_mins: 90,
                cause: StrandCause::NeverParked,
            }]
        );
    }

    /// THE FALSE-ALARM REGRESSION (e60398dc). The age was read off the
    /// gate-run's `opened_at` — when the GATE STARTED — so a gate that
    /// ran 30 minutes and went green 1 minute ago read as 31 minutes
    /// stranded, and a slower one crossed the threshold before auto-park
    /// could possibly have filed. Measured 2026-09-09: green gates run a
    /// median 18.9 min. Age is now dated from the VERDICT, so a green
    /// that just landed is fresh no matter how long its gate took.
    #[test]
    fn a_long_gate_that_just_went_green_is_not_stranded() {
        let now = Utc.with_ymd_and_hms(2026, 9, 7, 12, 0, 0).unwrap();
        let runs = [green_run(
            "gr-slow",
            "fix/slow-gate",
            // Opened 3h ago — under the old opened_at basis this alarms
            // instantly; the verdict landed 1 minute ago.
            "2026-09-07T09:00:00Z",
            Some("2026-09-07T11:59:00Z"),
            json!({}),
        )];
        assert!(
            stranded_greens_to_alarm(&runs, &branches(&[]), &branches(&[]), now, windows())
                .is_empty(),
            "a green one minute old is not stranded, however long its gate ran"
        );
    }

    /// A green with NO dateable verdict is left for a later pass rather
    /// than alarmed on the gate's start time: absence of a stamp is not
    /// evidence.
    #[test]
    fn a_green_with_no_verdict_stamp_is_left_alone() {
        let now = Utc.with_ymd_and_hms(2026, 9, 7, 12, 0, 0).unwrap();
        let runs = [green_run(
            "gr-nostamp",
            "fix/nostamp",
            "2026-09-07T06:00:00Z",
            None,
            json!({}),
        )];
        assert!(
            stranded_greens_to_alarm(&runs, &branches(&[]), &branches(&[]), now, windows())
                .is_empty()
        );
    }

    /// The run's own `closed_at` dates the verdict when a step stamp is
    /// missing — the outcome rule closes the packet a fraction of a
    /// second after the verdict lands.
    #[test]
    fn closed_at_dates_the_verdict_when_the_step_has_no_stamp() {
        let now = Utc.with_ymd_and_hms(2026, 9, 7, 12, 0, 0).unwrap();
        let runs = [green_run(
            "gr-closed",
            "fix/closed",
            "2026-09-07T06:00:00Z",
            None,
            json!({"closed_at": "2026-09-07T10:00:00Z"}),
        )];
        let got = stranded_greens_to_alarm(&runs, &branches(&[]), &branches(&[]), now, windows());
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].age_mins, 120);
    }

    /// A just-gated green is NOT stranded — its car is moments away
    /// (jobs.auto-park files it on the green event).
    #[test]
    fn a_fresh_green_inside_the_window_is_not_selected() {
        let now = Utc.with_ymd_and_hms(2026, 9, 7, 12, 0, 0).unwrap();
        let runs = [green_run(
            "gr-2",
            "fix/fresh",
            "2026-09-07T09:00:00Z",
            Some("2026-09-07T11:50:00Z"),
            json!({}),
        )];
        assert!(
            stranded_greens_to_alarm(&runs, &branches(&[]), &branches(&[]), now, windows())
                .is_empty()
        );
    }

    /// A green that CARRIED a park intent is judged on the auto-park
    /// grace, not the 45-minute human window: the handler files in 0.6
    /// SECONDS (measured 2026-09-09), so 20 minutes with no car is the
    /// handler having failed, and the packet says so.
    #[test]
    fn an_intent_carrying_green_alarms_on_the_grace_and_names_auto_park() {
        let now = Utc.with_ymd_and_hms(2026, 9, 7, 12, 0, 0).unwrap();
        let runs = [green_run_with_intent(
            "gr-intent",
            "fix/auto",
            "2026-09-07T11:20:00Z",
            Some("2026-09-07T11:40:00Z"),
        )];
        let got = stranded_greens_to_alarm(&runs, &branches(&[]), &branches(&[]), now, windows());
        assert_eq!(got.len(), 1, "20 min with no car is past the 10-min grace");
        assert_eq!(got[0].cause, StrandCause::AutoParkFailed);
        let body = stranded_alarm_body(&got[0], windows(), now);
        assert_eq!(body["metadata"]["stranded_cause"], "auto-park-failed");
        let title = body["title"].as_str().unwrap();
        assert!(title.contains("auto-park"), "the title names it: {title}");
    }

    /// Inside the grace, an intent-carrying green is not alarmed: a
    /// dispatcher restart or one redelivery must not read as a failure.
    #[test]
    fn an_intent_carrying_green_inside_the_grace_is_not_selected() {
        let now = Utc.with_ymd_and_hms(2026, 9, 7, 12, 0, 0).unwrap();
        let runs = [green_run_with_intent(
            "gr-intent-fresh",
            "fix/auto",
            "2026-09-07T11:20:00Z",
            Some("2026-09-07T11:55:00Z"),
        )];
        assert!(
            stranded_greens_to_alarm(&runs, &branches(&[]), &branches(&[]), now, windows())
                .is_empty()
        );
    }

    /// The other side of the same split: a HAND-gated green 20 minutes
    /// old is not alarmed — nothing is broken, and a human's
    /// gate-then-park gap is not a defect.
    #[test]
    fn a_hand_gated_green_gets_the_longer_window() {
        let now = Utc.with_ymd_and_hms(2026, 9, 7, 12, 0, 0).unwrap();
        let runs = [green_run(
            "gr-hand",
            "fix/hand",
            "2026-09-07T11:20:00Z",
            Some("2026-09-07T11:40:00Z"),
            json!({}),
        )];
        assert!(
            stranded_greens_to_alarm(&runs, &branches(&[]), &branches(&[]), now, windows())
                .is_empty()
        );
    }

    /// A green whose branch became a car is not stranded (the shared
    /// definition filters it) — proves the reuse of
    /// `census::stranded_gate_runs`.
    #[test]
    fn a_green_with_a_car_is_not_selected() {
        let now = Utc.with_ymd_and_hms(2026, 9, 7, 12, 0, 0).unwrap();
        let runs = [green_run(
            "gr-4",
            "fix/parked",
            "2026-09-07T10:00:00Z",
            Some("2026-09-07T10:10:00Z"),
            json!({}),
        )];
        assert!(
            stranded_greens_to_alarm(
                &runs,
                &branches(&["fix/parked"]),
                &branches(&[]),
                now,
                windows()
            )
            .is_empty()
        );
    }

    /// A green `jobs.auto-park` deliberately SKIPPED — the branch had
    /// landed, or its car was already aboard a train — is a recorded
    /// decision, not a strand. It never alarms.
    #[test]
    fn an_auto_park_skipped_green_is_not_selected() {
        let now = Utc.with_ymd_and_hms(2026, 9, 7, 12, 0, 0).unwrap();
        let runs = [green_run(
            "gr-skipped",
            "fix/landed",
            "2026-09-07T09:00:00Z",
            Some("2026-09-07T09:10:00Z"),
            json!({"park_summary": "does the thing", "park_skipped": "landed"}),
        )];
        assert!(
            stranded_greens_to_alarm(&runs, &branches(&[]), &branches(&[]), now, windows())
                .is_empty()
        );
    }

    /// A branch an open alarm already names is skipped — the dedup that
    /// stops a flood of one packet every ten minutes.
    #[test]
    fn an_already_alarmed_branch_is_deduped() {
        let now = Utc.with_ymd_and_hms(2026, 9, 7, 12, 0, 0).unwrap();
        let runs = [green_run(
            "gr-5",
            "fix/stranded",
            "2026-09-07T10:00:00Z",
            Some("2026-09-07T10:10:00Z"),
            json!({}),
        )];
        assert_eq!(
            stranded_greens_to_alarm(&runs, &branches(&[]), &branches(&[]), now, windows()).len(),
            1
        );
        assert!(
            stranded_greens_to_alarm(
                &runs,
                &branches(&[]),
                &branches(&["fix/stranded"]),
                now,
                windows()
            )
            .is_empty()
        );
    }

    /// TRUNCATION REGRESSION at the stranded-green dedup. The existing
    /// alarm for a strand can sit beyond a single page of open
    /// backlog-items; a bare `limit=200` read that treats the truncated
    /// page as the whole set misses it and re-files every pass (the
    /// notification flood). Building the dedup set through
    /// `list_all_pages` — as `alarm_stranded_greens` does — gathers the
    /// alarm on page three, so the strand is deduped.
    #[tokio::test]
    async fn a_dedup_alarm_beyond_the_first_page_still_dedups() {
        use super::{PAGE_LIMIT, list_all_pages};
        let now = Utc.with_ymd_and_hms(2026, 9, 7, 12, 0, 0).unwrap();
        let mut open: Vec<serde_json::Value> = (0..250)
            .map(|i| {
                json!({"id": format!("bi-{i}"),
                       "metadata": {"stranded_branch": format!("other/{i}")}})
            })
            .collect();
        open[220] = json!({"id": "bi-220", "metadata": {"stranded_branch": "fix/stranded"}});
        let open_ref = &open;
        let gathered = list_all_pages(|offset| async move {
            let page: Vec<serde_json::Value> = open_ref
                .iter()
                .skip(offset)
                .take(PAGE_LIMIT)
                .cloned()
                .collect();
            anyhow::Ok(Some(json!({
                "data": page, "total": open_ref.len(), "offset": offset, "limit": PAGE_LIMIT,
            })))
        })
        .await
        .unwrap();
        let already_alarmed: BTreeSet<String> = gathered
            .iter()
            .filter_map(|j| {
                j.get("metadata")?
                    .get("stranded_branch")?
                    .as_str()
                    .map(str::to_string)
            })
            .collect();
        let runs = [green_run(
            "gr-trunc",
            "fix/stranded",
            "2026-09-07T10:00:00Z",
            Some("2026-09-07T10:10:00Z"),
            json!({}),
        )];
        assert!(
            stranded_greens_to_alarm(&runs, &branches(&[]), &already_alarmed, now, windows())
                .is_empty(),
            "the strand's alarm sits on page three; the paginated read must find it and dedup"
        );
        let truncated: BTreeSet<String> = open
            .iter()
            .take(200)
            .filter_map(|j| {
                j.get("metadata")?
                    .get("stranded_branch")?
                    .as_str()
                    .map(str::to_string)
            })
            .collect();
        assert_eq!(
            stranded_greens_to_alarm(&runs, &branches(&[]), &truncated, now, windows()).len(),
            1,
            "a truncated 200-row read misses the alarm and re-files — the defect this fixes"
        );
    }

    /// A superseded green is dead, not waiting — it never alarms
    /// (rescue guidance pointing at a deleted branch is worse than
    /// none).
    #[test]
    fn a_superseded_green_is_not_selected() {
        let now = Utc.with_ymd_and_hms(2026, 9, 7, 12, 0, 0).unwrap();
        let runs = [green_run(
            "gr-6",
            "fix/superseded",
            "2026-09-07T10:00:00Z",
            Some("2026-09-07T10:10:00Z"),
            json!({"superseded": "by hand"}),
        )];
        assert!(
            stranded_greens_to_alarm(&runs, &branches(&[]), &branches(&[]), now, windows())
                .is_empty()
        );
    }

    /// A HELD green is deliberately waiting, and a RE-RAILED one is
    /// spent — neither alarms; the unheld green beside them still does.
    /// This is the drift that filed four false packets on 2026-09-09:
    /// the yard excluded both and the alarm did not.
    #[test]
    fn a_held_or_rerailed_green_is_not_selected() {
        let now = Utc.with_ymd_and_hms(2026, 9, 7, 12, 0, 0).unwrap();
        let runs = [
            green_run(
                "gr-7",
                "fix/held",
                "2026-09-07T10:00:00Z",
                Some("2026-09-07T10:10:00Z"),
                json!({"hold": "lands at the next restart"}),
            ),
            green_run(
                "gr-9",
                "fix/old",
                "2026-09-07T10:00:00Z",
                Some("2026-09-07T10:10:00Z"),
                json!({"rerailed_to": "fix/old-rerail"}),
            ),
            green_run(
                "gr-8",
                "fix/free",
                "2026-09-07T10:00:00Z",
                Some("2026-09-07T10:10:00Z"),
                json!({}),
            ),
        ];
        let out = stranded_greens_to_alarm(&runs, &branches(&[]), &branches(&[]), now, windows());
        assert_eq!(
            out.iter().map(|s| s.branch.as_str()).collect::<Vec<_>>(),
            vec!["fix/free"]
        );
    }

    /// The FRESHEST green among a branch's several runs decides age: a
    /// re-gate that just went green makes the branch fresh even if an
    /// older run for the same branch went green long ago.
    #[test]
    fn the_freshest_green_run_decides_age() {
        let now = Utc.with_ymd_and_hms(2026, 9, 7, 12, 0, 0).unwrap();
        let runs = [
            green_run(
                "gr-old",
                "fix/regated",
                "2026-09-06T00:00:00Z",
                Some("2026-09-06T01:00:00Z"),
                json!({}),
            ),
            green_run(
                "gr-new",
                "fix/regated",
                "2026-09-07T11:40:00Z",
                Some("2026-09-07T11:50:00Z"),
                json!({}),
            ),
        ];
        assert!(
            stranded_greens_to_alarm(&runs, &branches(&[]), &branches(&[]), now, windows())
                .is_empty(),
            "the branch was re-gated 10 min ago — fresh, not stranded"
        );
    }

    /// The alarm packet a strand becomes: a backlog-item carrying the
    /// dedup key `stranded_branch`, a stable title, and the rescue
    /// guidance an operator acts on.
    #[test]
    fn the_alarm_packet_carries_the_dedup_key_and_rescue() {
        let now = Utc.with_ymd_and_hms(2026, 9, 7, 12, 0, 0).unwrap();
        let a = StrandedGreen {
            branch: "fix/stranded".into(),
            gate_run_id: "gr-1".into(),
            age_mins: 90,
            cause: StrandCause::NeverParked,
        };
        let b = stranded_alarm_body(&a, windows(), now);
        assert_eq!(b["kind"], "backlog-item");
        assert_eq!(b["status"], "open");
        assert_eq!(b["priority"], "standard");
        assert_eq!(b["owner_id"], "emp-david");
        assert_eq!(b["metadata"]["stranded_branch"], "fix/stranded");
        assert_eq!(b["metadata"]["gate_run_id"], "gr-1");
        assert_eq!(b["metadata"]["verdict_age_mins"], 90);
        assert_eq!(b["metadata"]["stranded_cause"], "never-parked");
        let title = b["title"].as_str().unwrap();
        assert!(
            title.contains("fix/stranded"),
            "title names the branch: {title}"
        );
        let detail = b["metadata"]["detail"].as_str().unwrap();
        assert!(
            detail.contains("rebase") && detail.contains("re-gate"),
            "detail carries the orient rescue guidance: {detail}"
        );
    }

    /// THE HALF THAT WAS MISSING (e60398dc). An alarm whose branch
    /// stopped being stranded — it parked, landed, was held or was
    /// re-railed — is closed by the conductor itself. The alarm for a
    /// strand that still holds is left alone.
    #[test]
    fn an_alarm_whose_strand_ended_is_cleared_and_a_live_one_is_not() {
        let open = [
            json!({"id": "bi-gone", "metadata": {"stranded_branch": "fix/parked-since"}}),
            json!({"id": "bi-live", "metadata": {"stranded_branch": "fix/still-stranded"}}),
            // Not one of ours: no dedup key, so not this alarm's packet.
            json!({"id": "bi-other", "metadata": {"area": "pipeline"}}),
        ];
        let got = stranded_alarms_to_clear(&open, &branches(&["fix/still-stranded"]));
        assert_eq!(
            got,
            vec![("bi-gone".to_string(), "fix/parked-since".to_string())]
        );
    }

    /// The clear NAMES WHAT CHANGED rather than saying "no longer
    /// stranded" — a car now carries it, a marker spent it, or an
    /// operator held it.
    #[test]
    fn the_clear_names_what_ended_the_strand() {
        let runs = [
            green_run(
                "gr-a",
                "fix/held",
                "2026-09-07T10:00:00Z",
                Some("2026-09-07T10:10:00Z"),
                json!({"hold": "lands at the next restart"}),
            ),
            green_run(
                "gr-b",
                "fix/rerailed",
                "2026-09-07T10:00:00Z",
                Some("2026-09-07T10:10:00Z"),
                json!({"rerailed_to": "fix/rerailed-rerail"}),
            ),
        ];
        let cars = branches(&["fix/parked"]);
        assert!(stranded_clear_reason(&runs, &cars, "fix/parked").contains("car now carries"));
        assert!(stranded_clear_reason(&runs, &cars, "fix/rerailed").contains("rerailed_to"));
        assert!(stranded_clear_reason(&runs, &cars, "fix/held").contains("HELD"));
    }

    /// The clear completes the triage step as `stale` — the
    /// backlog-item terminal titled "Closed — the claim no longer
    /// holds" — carries the step's existing keys through the PUT, and
    /// STAMPS ITSELF, so a machine clear is distinguishable from a
    /// human's answer and can never be read as one.
    #[test]
    fn the_clear_step_body_closes_stale_and_stamps_itself() {
        let mut existing = Map::new();
        existing.insert("authority_role".into(), json!("platform-admin"));
        let body = stranded_clear_step_body(&existing, "fix/parked", "a car now carries it");
        assert_eq!(body["status"], "completed");
        assert_eq!(body["metadata"]["disposition"], "stale");
        assert_eq!(body["metadata"]["authority_role"], "platform-admin");
        assert_eq!(
            body["metadata"]["cleared_by"],
            json!(super::STRANDED_CLEARED_BY)
        );
        let evidence = body["metadata"]["evidence"].as_str().unwrap();
        assert!(
            evidence.contains("fix/parked") && evidence.contains("a car now carries it"),
            "the evidence names the branch and what changed: {evidence}"
        );
    }

    /// A STANDING alarm is refreshed with today's measurement rather
    /// than twinned — the metadata PATCH merges top-level keys.
    #[test]
    fn a_standing_alarm_is_refreshed_with_the_fresh_measurement() {
        let now = Utc.with_ymd_and_hms(2026, 9, 7, 12, 0, 0).unwrap();
        let a = StrandedGreen {
            branch: "fix/stranded".into(),
            gate_run_id: "gr-2".into(),
            age_mins: 300,
            cause: StrandCause::AutoParkFailed,
        };
        let patch = stranded_refresh_patch(&a, now);
        assert_eq!(patch["verdict_age_mins"], 300);
        assert_eq!(patch["gate_run_id"], "gr-2");
        assert_eq!(patch["stranded_cause"], "auto-park-failed");
        assert!(
            patch["last_measured_at"]
                .as_str()
                .unwrap()
                .starts_with("2026-09-07")
        );
        assert!(
            patch.get("stranded_branch").is_none(),
            "the dedup key never moves — a refresh must not repoint the packet"
        );
    }
}

#[cfg(test)]
mod no_departure_tests {
    use super::{
        CONDUCTOR_LOCK_WAIT, LOCK_CONTENDED_EXIT, NoDeparture, PREFLIGHT_FAIL_EXIT, Phase,
        lock_acquired_line, lock_contended_line, lock_wait_budget, lock_waiting_line,
        no_departure_line,
    };
    use std::time::Duration;

    /// THE FLOOD, in one assertion. Ten hours on 2026-09-10 (4860aff8):
    /// one car the consist check refused, a board firing every 60
    /// seconds, and every attempt opened a pr-train Job and cancelled
    /// it — 982 trains, the newest 100 closed inside 99 minutes, 100 of
    /// 100 with no `boarded_jobs`. The refusal already spends no PR and
    /// no CI; it must spend no PACKET either, and the journal line is
    /// now the only place an operator learns a window refused, so it
    /// has to say all three.
    #[test]
    fn a_refused_consist_opens_no_train_packet_and_says_so() {
        let line = no_departure_line(&NoDeparture::ConsistRefused {
            reason: "consist check: the-live-rules-are-the-authored-rules failed".to_string(),
            cars: 3,
        });
        assert!(line.starts_with("no train departed —"), "{line}");
        assert!(
            line.contains("the-live-rules-are-the-authored-rules"),
            "the refusal names the check that refused: {line}"
        );
        assert!(
            line.contains("No train packet opened"),
            "an operator must not go looking for a train that was never opened: {line}"
        );
        assert!(
            line.contains("3 car(s) stay boardable"),
            "a combination failure strikes nobody: {line}"
        );
    }

    /// An idle window is not a failure, and it is not a train either.
    #[test]
    fn an_idle_window_departs_nothing_and_opens_nothing() {
        let line = no_departure_line(&NoDeparture::NothingParked);
        assert!(line.starts_with("no train departed —"), "{line}");
        assert!(line.contains("not a failure"), "{line}");
        assert!(line.contains("No train packet opened"), "{line}");
    }

    /// Every candidate conflicted: the cars carry their own
    /// `skip_reason`, and the line names them so the journal answers
    /// "which branches" without a second read.
    #[test]
    fn an_all_conflicted_window_names_the_branches() {
        let line = no_departure_line(&NoDeparture::AllConflicted {
            branches: "fix/a, fix/b".to_string(),
        });
        assert!(line.starts_with("no train departed —"), "{line}");
        assert!(line.contains("fix/a, fix/b"), "{line}");
        assert!(line.contains("No train packet opened"), "{line}");
    }

    /// An infrastructure refusal is not a consist failure — it says so,
    /// and it carries the host reason the readiness check measured.
    #[test]
    fn a_host_refusal_carries_the_measured_reason() {
        let line = no_departure_line(&NoDeparture::HostShort {
            reason: "forge: 3.1 GB free, floor is 20 GB".to_string(),
        });
        assert!(line.starts_with("no train departed —"), "{line}");
        assert!(line.contains("3.1 GB free"), "{line}");
        assert!(
            line.contains("before any car was collected"),
            "nobody's car is at fault: {line}"
        );
    }

    /// A lock whose loser logs and leaves is correct ONLY if it
    /// eventually wins. The board fires every 60 seconds and holds the
    /// lock 12–14 seconds in the consist check; the 10-minute reconcile
    /// fired about a second later and lost 55 times in a row, so no
    /// merge, no stall sentinel, no arrival report and no branch sweep
    /// ran for nine hours. The starvable side waits; the side that must
    /// never queue behind a 30-minute deploy does not.
    #[test]
    fn only_the_starvable_phases_wait_for_the_lock() {
        assert_eq!(lock_wait_budget(&Phase::Reconcile), CONDUCTOR_LOCK_WAIT);
        assert_eq!(lock_wait_budget(&Phase::Run), CONDUCTOR_LOCK_WAIT);
        assert_eq!(
            lock_wait_budget(&Phase::Board),
            Duration::ZERO,
            "a board must not queue behind a long reconcile — its firing records \
             boarded-nothing and the next tick re-fires"
        );
        assert_eq!(lock_wait_budget(&Phase::Preflight), Duration::ZERO);
        assert!(
            CONDUCTOR_LOCK_WAIT >= Duration::from_secs(60),
            "the budget has to outlast a board that departs a train, not just one that refuses"
        );
    }

    /// The three lines a contended lock can leave. Each names the
    /// waited time, because "leaving" with no number is what made nine
    /// hours of starvation look like nine hours of 0-second successes.
    #[test]
    fn the_lock_lines_name_the_waited_time() {
        let waiting = lock_waiting_line(Duration::from_secs(120));
        assert!(waiting.contains("120s"), "{waiting}");
        let acquired = lock_acquired_line(Duration::from_secs(13));
        assert!(acquired.contains("13s"), "{acquired}");
        // The zero-wait case keeps the line every journal reader and
        // cadence.rs's own doc comment already greps for.
        let left_at_once = lock_contended_line(Duration::ZERO);
        assert_eq!(
            left_at_once, "another conductor run holds the lock — leaving",
            "the no-wait line is unchanged"
        );
        // A zero-budget phase still burns nanoseconds between reading
        // the clock and failing the try — that is not a wait, and the
        // line must not claim one.
        assert_eq!(
            lock_contended_line(Duration::from_nanos(400)),
            left_at_once,
            "a sub-second elapsed is not a wait"
        );
        let timed_out = lock_contended_line(Duration::from_secs(120));
        assert!(
            timed_out.contains("120s") && timed_out.contains("holds the lock"),
            "{timed_out}"
        );
        assert_ne!(
            LOCK_CONTENDED_EXIT, 0,
            "a pass that waited its whole budget and still never ran must not record rc=0"
        );
        assert_ne!(
            LOCK_CONTENDED_EXIT, PREFLIGHT_FAIL_EXIT,
            "preflight failure already owns its code — two causes must not share one exit"
        );
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
#[cfg(test)]
mod boards_after_tests {
    use super::{
        EDGE_HOLD, EDGE_HOLD_NEEDS_HUMAN, EDGE_HOLD_WAITING, EdgeOutcome, NoDeparture, Predecessor,
        boards_after_outcome, declared_predecessor, empty_dock_refusal, no_departure_line,
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
    }

    /// "NO SUCH JOB" AND "I COULD NOT ASK" MUST NOT BE CONFUSED: the
    /// first holds a car for a person to fix, the second boards it. The
    /// message below is built the way `api_once` builds it, which is what
    /// keeps this classification honest while `api` still flattens its
    /// `Failure` into an `anyhow::Error` (see `is_no_such_job`).
    #[test]
    fn a_404_is_read_back_out_of_the_message_api_once_builds() {
        // Verbatim shape from `api_once`:
        //   anyhow!("{method} {path}: HTTP {status}: {}", body.trim())
        let not_found = anyhow::anyhow!(
            "GET /api/jobs/{PRED}: HTTP 404 Not Found: {{\"error\":\"job not found\"}}"
        );
        assert!(super::is_no_such_job(&not_found));

        for blip in [
            anyhow::anyhow!("GET /api/jobs/{PRED}: HTTP 503 Service Unavailable: "),
            anyhow::anyhow!("error sending request for url (http://sor:7900/api/jobs/x)")
                .context("GET /api/jobs/x"),
            anyhow::anyhow!("job {PRED} came back empty"),
        ] {
            assert!(
                !super::is_no_such_job(&blip),
                "a blip must never read as an absence — it would hold a car for a person \
                 over a transient: {blip}"
            );
        }
    }

    /// An empty dock with no edge holds is still the idle window it
    /// always was — the pre-existing line, unchanged.
    #[test]
    fn an_empty_dock_with_no_edge_hold_is_an_idle_window() {
        assert_eq!(empty_dock_refusal(&[]), NoDeparture::NothingParked);
        let other = json!({"car_id_short": "aaaaaaaa", "reason": "branch fix/x not on fork"});
        assert_eq!(
            empty_dock_refusal(&[other]),
            NoDeparture::NothingParked,
            "a left-behind for some other reason is not an edge hold"
        );
    }

    /// A DOCK FULL OF HELD CARS IS NOT AN IDLE WINDOW, and the window's
    /// own line has to say which half of the hold it is — that is the
    /// decision an operator is reading it to make.
    #[test]
    fn an_empty_dock_held_on_edges_says_whether_a_human_is_needed() {
        let waiting = json!({"car_id_short": "aaaaaaaa", EDGE_HOLD: EDGE_HOLD_WAITING});
        let stuck = json!({"car_id_short": "cccccccc", EDGE_HOLD: EDGE_HOLD_NEEDS_HUMAN});

        let self_clearing = empty_dock_refusal(std::slice::from_ref(&waiting));
        assert_eq!(
            self_clearing,
            NoDeparture::HeldOnEdges {
                cars: "aaaaaaaa".into(),
                needs_human: String::new()
            }
        );
        let line = no_departure_line(&self_clearing);
        assert!(line.contains("no train departed"), "greppable: {line}");
        assert!(line.contains("NOTHING NEEDS DOING"), "{line}");
        assert!(line.contains("aaaaaaaa"), "name the car: {line}");

        let needs_human = empty_dock_refusal(&[waiting, stuck]);
        let line = no_departure_line(&needs_human);
        assert!(line.contains("A HUMAN IS NEEDED for cccccccc"), "{line}");
        assert!(
            !line.contains("NOTHING NEEDS DOING"),
            "one stuck car means the window is not self-clearing: {line}"
        );
        assert!(
            line.contains("aaaaaaaa"),
            "the waiting car is still listed as held: {line}"
        );
    }
}

// ---------------------------------------------------------------------------
// The dock pin — `parked_ready` against the SEEDED loading-dock row.
// ---------------------------------------------------------------------------

/// "Is this car parked and ready to board?" is answered twice: here, by
/// the conductor, and by the `loading-dock` station row the yard, the
/// `/queue` endpoint and `boss orient` all read. On 2026-09-10 they
/// disagreed — the row had no hold term, so `/api/yard/status` reported
/// `dock_depth: 2, threshold_met: true` over two cars that could not
/// board (backlog 36c3d4ca). This is the equality test CLAUDE.md §9a
/// asks for while the duplication stands.
///
/// **A PIN IS A HOLDING ACTION, NOT A DESTINATION.** The destination is
/// the conductor reading the row instead of carrying its own copy —
/// `GET /api/stations/loading-dock/queue` already serves exactly this
/// predicate. That is not this car: `parked_ready` is a pure
/// `fn(&Value) -> bool` called from the boarding collector AND the
/// cadence loop's depth probe, and making it HTTP-bound puts every
/// landing behind a new network dependency, which is the failure mode
/// "a conductor loop write must not be fatal" was written about. So both
/// predicates stay, and this test refuses to let them drift again.
#[cfg(test)]
mod dock_pin {
    use super::parked_ready;
    use boss_core::job::{Job, JobStatus, Priority, Step, StepStatus, Subject};
    use boss_jobs::{PgStations, StationRegistry};
    use serde_json::Value;

    /// ONE fixture, both shapes. The typed pair feeds the station
    /// predicate; its own serialisation feeds `parked_ready`, which
    /// reads a car as JSON off the API. Deriving the JSON rather than
    /// hand-writing it is the point — a hand-written copy can disagree
    /// with the types and the test would still pass.
    fn car(
        branch: &str,
        review: StepStatus,
        review_metadata: Value,
        train: Option<&str>,
    ) -> (Job, Vec<Step>) {
        let mut job = Job::new(
            "ship-a-change",
            Subject::new("custom", branch),
            format!("car {branch}"),
            "emp-1",
            Priority::Standard,
            chrono::NaiveDate::from_ymd_opt(2026, 9, 10).unwrap(),
        );
        job.status = JobStatus::Open;
        let mut metadata = serde_json::json!({ "branch": branch });
        if let Some(t) = train {
            metadata["train"] = serde_json::json!(t);
        }
        job.metadata = metadata;
        let mut step = Step::new(job.id, "task", "Open for review", 0);
        step.spec_slug = Some("review".to_string());
        step.status = review;
        step.metadata = review_metadata;
        (job, vec![step])
    }

    fn as_api_json(job: &Job, steps: &[Step]) -> Value {
        let mut v = serde_json::to_value(job).expect("a Job serialises");
        v["steps"] = serde_json::to_value(steps).expect("steps serialise");
        v
    }

    async fn seeded_dock(db: &boss_testing::TestDb) -> boss_jobs::StationSpec {
        PgStations::new(db.pool.clone())
            .get_active("loading-dock")
            .await
            .expect("the schema seeds one active loading-dock row")
    }

    /// Every case the dock actually holds, and the answer BOTH readers
    /// must give. Two deliberate differences are left out and named in
    /// the test below; they are the measure of what a full collapse
    /// would buy.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_seeded_dock_row_and_the_conductor_agree_on_every_car() {
        let db = boss_testing::TestDb::new().await;
        let spec = seeded_dock(&db).await;

        let cases: Vec<(&str, bool, (Job, Vec<Step>))> = vec![
            (
                "parked, nothing written on the review step",
                true,
                car("feat/a", StepStatus::Ready, serde_json::json!({}), None),
            ),
            (
                "parked and claimed — active is boardable too",
                true,
                car("feat/b", StepStatus::Active, serde_json::json!({}), None),
            ),
            (
                "held with a reason",
                false,
                car(
                    "feat/c",
                    StepStatus::Ready,
                    serde_json::json!({"hold": "waiting on a kubectl delete"}),
                    None,
                ),
            ),
            (
                "held with no reason recorded",
                false,
                car(
                    "feat/d",
                    StepStatus::Ready,
                    serde_json::json!({"hold": true}),
                    None,
                ),
            ),
            // THE TRAP: a released hold is `false`, not a deleted key.
            (
                "hold released by writing false",
                true,
                car(
                    "feat/e",
                    StepStatus::Ready,
                    serde_json::json!({"hold": false}),
                    None,
                ),
            ),
            (
                "hold released by blanking the field",
                true,
                car(
                    "feat/f",
                    StepStatus::Ready,
                    serde_json::json!({"hold": ""}),
                    None,
                ),
            ),
            (
                "metadata that says nothing about holding",
                true,
                car(
                    "feat/g",
                    StepStatus::Ready,
                    serde_json::json!({"note": "looks fine"}),
                    None,
                ),
            ),
            (
                "already aboard a train",
                false,
                car(
                    "feat/h",
                    StepStatus::Ready,
                    serde_json::json!({}),
                    Some("train-job-id"),
                ),
            ),
            (
                "review completed — past the dock",
                false,
                car("feat/i", StepStatus::Completed, serde_json::json!({}), None),
            ),
            (
                "review not yet ready",
                false,
                car("feat/j", StepStatus::Pending, serde_json::json!({}), None),
            ),
        ];

        for (what, expected, (job, steps)) in &cases {
            let conductor = parked_ready(&as_api_json(job, steps));
            assert_eq!(
                conductor, *expected,
                "`parked_ready` is wrong about a car {what}"
            );
            assert_eq!(
                spec.predicate.matches(job, steps),
                *expected,
                "the seeded loading-dock predicate is wrong about a car {what} \
                 — the row and the conductor have drifted, which is backlog \
                 36c3d4ca happening again"
            );
        }
    }

    /// THE TWO KNOWN DIFFERENCES, written as a test so they cannot be
    /// forgotten, and both unreachable for a real car:
    ///
    /// - a `train/` branch. `parked_ready` refuses one ("a consist is
    ///   not a car"); the predicate language has no prefix clause. It
    ///   cannot matter, because the row's `kind: ship-a-change` clause
    ///   never admits a consist — a train is a `pr-train` packet.
    /// - a blank `branch`. `metadata_present` means "present and
    ///   non-null", not "non-blank", so the row counts a car whose
    ///   branch is `""` and the conductor does not. Nothing writes one:
    ///   `boss park` and the auto-park handler both take the branch from
    ///   the gate receipt.
    ///
    /// Closing either means widening the predicate language again, which
    /// is worth doing only once a real car lands in one of these states.
    /// If one does, this test is where the evidence goes.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_differences_that_remain_are_unreachable() {
        let db = boss_testing::TestDb::new().await;
        let spec = seeded_dock(&db).await;

        let (consist, steps) = car(
            "train/20260910-2000",
            StepStatus::Ready,
            serde_json::json!({}),
            None,
        );
        assert!(!parked_ready(&as_api_json(&consist, &steps)));
        assert!(
            spec.predicate.matches(&consist, &steps),
            "documented difference: the row has no branch-prefix clause"
        );
        let mut pr_train = consist.clone();
        pr_train.kind = "pr-train".to_string();
        assert!(
            !spec.predicate.matches(&pr_train, &steps),
            "the `kind` clause is what makes that difference unreachable: a real \
             consist is a pr-train packet and the dock never admits one"
        );

        let (blank, steps) = car("", StepStatus::Ready, serde_json::json!({}), None);
        assert!(!parked_ready(&as_api_json(&blank, &steps)));
        assert!(
            spec.predicate.matches(&blank, &steps),
            "documented difference: `metadata_present` means non-null, not non-blank"
        );
    }
}
