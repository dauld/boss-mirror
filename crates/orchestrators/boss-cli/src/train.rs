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
//!  2. BOARD — open this window's train Job, collect the ship-a-change
//!     Jobs that are ready (review step ready/active, a branch pushed to
//!     the fork, not already on a train), assemble one train branch by
//!     merging each on top of origin/main, run the CONSIST CHECK over
//!     the assembled tree (`consist_check` — seconds of cheap text lints,
//!     because a per-branch gate cannot see a failure that exists only in
//!     the combination), push it, open ONE batched PR.
//!     A branch that does not merge cleanly is skipped, named on the Job,
//!     and left for the next train. An empty window — or a consist the
//!     check refused — cancels the train via the `job.metadata.empty`
//!     marker rather than pretending, and a refusal strikes no car.
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

use std::collections::BTreeSet;
use std::fs::{self, File, TryLockError};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;
use boss_jobs::delivery::DeliveryPolicyRow;
use chrono::{DateTime, FixedOffset, Utc};
use reqwest::Method;
use serde_json::{Map, Value, json};

use crate::delivery_policy::{self, DeliveryPolicy};
use crate::host_readiness;

const ACTOR: &str = "automation:train-conductor";

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

/// The identity every verb acts as — for callers that need the bare
/// id (e.g. `boss job file` stamping `owner_id`) rather than the
/// serialized header.
pub(crate) fn actor_id() -> &'static str {
    ACTOR
}

pub(crate) fn boss_user() -> String {
    json!({
        "id": ACTOR, "role": "platform-admin", "access_tier": "operator",
        "territory_account_ids": [], "direct_report_ids": [], "department": "platform",
    })
    .to_string()
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

/// The list body, whether or not the endpoint wrapped it in
/// `{"data": [...]}`.
/// The train holding the track, named for the journal, or None when
/// the track is clear. Any open pr-train packet occupies it: arrived
/// and cancelled trains are closed, so both clear the track — a red
/// train that stall-cancels never blocks the next one.
pub(crate) fn track_occupied_by(open_trains: &[Value]) -> Option<String> {
    open_trains.first().map(|t| {
        let title = t
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or("an unnamed train");
        let id = t.get("id").and_then(Value::as_str).unwrap_or("?");
        format!("{title} ({})", &id[..id.len().min(8)])
    })
}

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
    !truthy(
        review
            .and_then(|s| s.get("metadata"))
            .and_then(|m| m.get("hold")),
    )
}

/// The branch-sweep decision at arrival (protocol decision, David):
/// train PRs squash-merge, so git ancestry can never prove a car's
/// content landed — the JOB RECORD is the proof. Given the cars a
/// landed train boarded and the branches still-open cars name, a
/// car's branch is deletable iff:
///   - the car's own bookkeeping completed: closed with the `merged`
///     outcome (an abandoned car closes too, but its branch holds
///     unmerged work — never touch it);
///   - the branch is named and is not `main`;
///   - no still-open car rides the same branch (a follow-up car's
///     claim keeps it alive).
/// Two landed cars naming one branch delete it once. Pure — the
/// forge call and the journal line belong to the caller.
pub(crate) fn deletable_branches(
    boarded_cars: &[Value],
    open_branches: &BTreeSet<String>,
) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for car in boarded_cars {
        let Some(cid) = car.get("id").and_then(Value::as_str) else {
            continue;
        };
        let md = car.get("metadata");
        let branch = md
            .and_then(|m| m.get("branch"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        let landed = car.get("status").and_then(Value::as_str) == Some("closed")
            && md.and_then(|m| m.get("outcome")).and_then(Value::as_str) == Some("merged");
        if branch.is_empty()
            || branch == "main"
            || !landed
            || open_branches.contains(branch)
            || out.iter().any(|(b, _)| b == branch)
        {
            continue;
        }
        out.push((branch.to_string(), cid.to_string()));
    }
    out
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
pub(crate) fn sweep_note(guard: &SweepGuard, branch: &str, car: &str) -> Option<String> {
    match guard {
        SweepGuard::Gone | SweepGuard::Delete => None,
        SweepGuard::NoRecord => Some(format!(
            "branch {branch} has no boarded head on record — not deleting (car {} landed)",
            id8(car)
        )),
        SweepGuard::Moved { recorded, current } => {
            Some(branch_moved_line(branch, recorded, current))
        }
    }
}

/// The line the sweep journals when a branch outgrew its boarding —
/// operator surface, and the only notice that unmerged commits are
/// sitting on a branch the train did not carry.
pub(crate) fn branch_moved_line(branch: &str, recorded: &str, current: &str) -> String {
    format!(
        "branch {branch} moved since boarding ({} -> {}) — not deleting",
        id8(recorded),
        id8(current)
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
                rollup.push(json!({
                    // The check's NAME. `ci_check_summary` has always
                    // rendered `context:STATE` and the completing step
                    // has always been commented "WHICH check, not just
                    // that one failed" — but this adapter dropped the
                    // field, so every red train in the SoR read
                    // `?:FAILURE`. On 2026-09-02 that cost a trip to
                    // the forge API to learn the answer was `test`, and
                    // another to learn `test` had died on a disk floor,
                    // not on any code. A verdict that cannot name what
                    // failed is a verdict someone has to go re-derive.
                    "context": st.get("context").and_then(Value::as_str).unwrap_or_default(),
                    // The forge's own one-line reason, when it gives
                    // one — free provenance for the same price.
                    "description": st
                        .get("description")
                        .and_then(Value::as_str)
                        .unwrap_or_default(),
                    "conclusion": conclusion,
                    "status": if verdict == "pending" { "PENDING" } else { "COMPLETED" },
                }));
            }
        }
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

    /// Record why the machine abandoned this window, on the same
    /// `cancelled` terminal a human's `--reason` fills.
    ///
    /// The terminal fires off the `empty` predicate, so until now a
    /// SELF-cancellation completed it with no reason at all. Measured
    /// 2026-09-05: of 16 cancelled trains, the 11 a human cancelled
    /// all carry a reason and the 5 the MACHINE cancelled carry none —
    /// including both of the previous night's consist refusals, which
    /// is why a jammed yard could only be explained by reading the
    /// conductor's pod log.
    ///
    /// MTTR is detection plus fix, and the fixes have been quick; the
    /// hours went to finding out. A refusal that leaves no record on
    /// the packet it refused is the detection cost in its purest form.
    async fn record_abandon_reason(&self, train: &Value, reason: &str) -> Result<()> {
        self.complete_step(
            train,
            find_step(train, "cancelled", "Cancelled — nothing to board"),
            &[("reason", Some(reason.to_string()))],
        )
        .await
    }

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
        let trains = rows(
            self.api(
                Method::GET,
                "/api/jobs?kind=pr-train&status=open&limit=50",
                None,
            )
            .await?,
        )?;
        for t0 in trains {
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
                continue; // this window's board phase, or a stalled assembly
            }
            let pr_url = pr_step
                .and_then(|s| s.get("metadata"))
                .and_then(|m| m.get("pr_url"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            if pr_url.is_empty() {
                continue;
            }
            let mut info = self.forge.pr_info(&pr_url).await?;

            let ci_step = find_step(&t, "ci", "CI verdict");
            let verdict = ci_verdict(info.get("statusCheckRollup"));
            if !step_done(ci_step) && verdict != "pending" {
                let checks = ci_check_summary(info.get("statusCheckRollup"));
                self.complete_step(
                    &t,
                    ci_step,
                    &[
                        ("result", Some(verdict.to_string())),
                        // WHICH check, not just that one failed.
                        ("checks", (!checks.is_empty()).then_some(checks)),
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
                continue;
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
                self.complete_step(&t, merged_step, &[("merge_ref", Some(merge_ref.clone()))])
                    .await?;
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
                for cid in boarded {
                    // The car's review closes HERE, not at boarding —
                    // the change was open for review until it landed,
                    // and leaving the step ready while the car rides is
                    // what lets a cancelled train release it (see the
                    // boarding loop). Completed first, because the
                    // ship-a-change spec gates `merged` on
                    // `steps.review.done AND job.metadata.merged`, and
                    // the marker below is what the dispatcher watches to
                    // close the Job.
                    let car = self.get_job(&cid).await?;
                    let review = find_step(&car, "review", "Open for review");
                    if !step_done(review) {
                        self.complete_step(
                            &car,
                            review,
                            &[
                                ("pr_url", Some(pr_url.clone())),
                                ("note", Some(format!("landed on main as {merge_ref}"))),
                            ],
                        )
                        .await?;
                    }
                    // v3 ship-a-change gates `merged` on this marker; the
                    // dispatcher closes the Job once it is set.
                    self.merge_job_metadata(
                        &cid,
                        vec![
                            ("merged", json!("true")),
                            ("merge_ref", json!(merge_ref.as_str())),
                        ],
                    )
                    .await?;
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
        let listed = rows(
            self.api(
                Method::GET,
                "/api/jobs?kind=ship-a-change&status=open&limit=100",
                None,
            )
            .await?,
        )?;
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
        // One fetch brings main + every parked branch into temp refs the
        // trial merges can address; refs/preview/* is cleaned each tick
        // so a deleted branch does not linger as a phantom.
        let dir = Some(Path::new(clone.as_str()));
        let mut args_owned: Vec<String> = vec![
            "git".into(),
            "fetch".into(),
            "--quiet".into(),
            "origin".into(),
            "+refs/heads/main:refs/preview/main".into(),
        ];
        for (_, _, b) in &cars {
            args_owned.push(format!("+refs/heads/{b}:refs/preview/{b}"));
        }
        let args: Vec<&str> = args_owned.iter().map(String::as_str).collect();
        sh_in(dir, true, &args)?;
        let rev = |r: &str| -> Result<String> {
            let out = sh_in(dir, true, &["git", "rev-parse", r])?;
            Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
        };
        let main_sha = rev("refs/preview/main")?;
        let mut pairs: Vec<(String, String)> = Vec::new();
        for (_, _, b) in &cars {
            pairs.push((b.clone(), rev(&format!("refs/preview/{b}"))?));
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
    /// about it (see `sweep_note`). A train whose cars have all
    /// reached a terminal is stamped `branches_swept`, so the steady
    /// state costs one list call and no per-car fetches.
    ///
    /// Forge cost: one list call, plus per UNSWEPT train one fetch
    /// per boarded car, one `branch_head` per deletable branch, and
    /// one delete of the train's own branch (a silent 404 once it is
    /// gone). The `branches_swept` stamp is what bounds it — coverage
    /// is never capped, so no landed branch goes uninspected.
    async fn sweep_landed_branches(&self) -> Result<()> {
        let arrived = rows(
            self.api(
                Method::GET,
                "/api/jobs?kind=pr-train&status=closed&limit=50",
                None,
            )
            .await?,
        )?;
        // Filter on the list rows (they carry metadata): swept trains
        // and cancelled ones (nothing boarded) drop out fetch-free.
        let pending: Vec<&Value> = arrived
            .iter()
            .filter(|t| {
                let md = t.get("metadata");
                !truthy(md.and_then(|m| m.get("branches_swept")))
                    && truthy(md.and_then(|m| m.get("boarded_jobs")))
            })
            .collect();
        if pending.is_empty() {
            return Ok(());
        }
        // Branches still-open cars name, fetched once per pass: a
        // live car's claim beats any landed car's deletion.
        let open_branches = self.open_car_branches().await?;
        for t in pending {
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
            for (branch, car) in deletable_branches(&cars, &open_branches) {
                // The job record proved the CONTENT landed; the head
                // guard proves the branch still holds only that
                // content. Both, or the branch stays (car 23923b40).
                let recorded = cars
                    .iter()
                    .find(|c| c.get("id").and_then(Value::as_str) == Some(car.as_str()))
                    .and_then(boarded_head)
                    .map(str::to_string);
                let current = self.forge.branch_head(&branch).await?;
                let guard = sweep_guard(recorded.as_deref(), current.as_deref());
                // Verdicts that keep a branch narrate themselves, and
                // a branch already off the forge narrates nothing.
                if let Some(note) = sweep_note(&guard, &branch, &car) {
                    log(note);
                }
                if guard == SweepGuard::Delete {
                    if self.cfg.dry {
                        log(format!(
                            "DRY: would delete branch {branch} (car {} landed)",
                            id8(&car)
                        ));
                    } else if self.forge.delete_branch(&branch).await? {
                        log(format!(
                            "deleted branch {branch} (car {} landed)",
                            id8(&car)
                        ));
                    } else {
                        // It existed a moment ago — something else
                        // swept it between the two calls. Rare, and
                        // worth saying so it is not read as our doing.
                        log(format!(
                            "branch {branch} already gone (car {} landed)",
                            id8(&car)
                        ));
                    }
                }
            }
            if sweep_settled(&cars) {
                self.merge_job_metadata(tid, vec![("branches_swept", json!("true"))])
                    .await?;
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
        let listed = rows(
            self.api(
                Method::GET,
                "/api/jobs?kind=ship-a-change&status=open&limit=100",
                None,
            )
            .await?,
        )?;
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
        let listed = rows(
            self.api(
                Method::GET,
                "/api/jobs?kind=ship-a-change&status=open&limit=100",
                None,
            )
            .await?,
        )?;
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

        // THE TRACK — one train at a time. The cadence loop holds a
        // departure before it claims a window (cadence::decide), so
        // this is the backstop for a hand-run `boss train board` and
        // for two conductors racing: an open pr-train packet means the
        // previous train has not arrived or cancelled, and a second
        // consist assembled now would merge onto a main the first is
        // about to change (a8c6773b). No packet is opened for a hold —
        // it is not a refusal, the yard is not empty, and the next tick
        // after the track clears departs.
        let on_track = rows(
            self.api(
                Method::GET,
                "/api/jobs?kind=pr-train&status=open&limit=5",
                None,
            )
            .await?,
        )?;
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
                log(format!("BOARDING REFUSED — {reason}"));
                // Recorded the way an empty window records itself: the
                // train Job opens, carries the reason on its collect
                // step, and cancels via the `empty` marker — so the
                // yard shows WHY no train ran instead of showing
                // nothing at all.
                let Some(train) = self.open_train_job(&train_branch, &window).await? else {
                    return Ok(()); // dry run — the refusal is on the journal
                };
                let train_id = job_id(&train)?.to_string();
                let collect = find_step(&train, "collect", "Collect what is ready to board");
                self.merge_job_metadata(&train_id, vec![("empty", json!("true"))])
                    .await?;
                let abandon_reason =
                    format!("boarding refused before any car was collected — {reason}");
                self.complete_step(
                    &train,
                    collect,
                    &[("boarded", Some(format!("nothing boarded — {reason}")))],
                )
                .await?;
                self.record_abandon_reason(&train, &abandon_reason).await?;
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
        let Some(train) = self.open_train_job(&train_branch, &window).await? else {
            // dry run
            log(format!("DRY: candidates: {}", py_pairs(&cands)));
            return Ok(());
        };
        let train_id = job_id(&train)?.to_string();
        let collect = find_step(&train, "collect", "Collect what is ready to board");

        if cands.is_empty() {
            self.merge_job_metadata(&train_id, vec![("empty", json!("true"))])
                .await?;
            let abandon_reason =
                "no car was parked and ready when the window opened — an idle window, not a failure"
                    .to_string();
            self.complete_step(
                &train,
                collect,
                &[(
                    "boarded",
                    Some("nothing ready to board this window".to_string()),
                )],
            )
            .await?;
            self.record_abandon_reason(&train, &abandon_reason).await?;
            log("empty window — train cancels via the marker");
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
            self.merge_job_metadata(&train_id, vec![("empty", json!("true"))])
                .await?;
            self.complete_step(
                &train,
                collect,
                &[(
                    "boarded",
                    Some(format!(
                        "all candidates skipped on merge conflicts: {skipped_names}"
                    )),
                )],
            )
            .await?;
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
            // together. So: no PR, no push, no CI spent — and every car
            // keeps `metadata.train` unset (never boarded, so still
            // `parked_ready`) and `red_trains` untouched. Striking cars
            // for a combination failure is the bug we already know
            // about; the only thing they carry away is the reason,
            // which names the check and the files it complained about.
            let mut left_boardable = Vec::with_capacity(boarded.len());
            for (j, _branch, _head) in &boarded {
                let cid = job_id(j)?;
                left_behind.push(json!({
                    "car_id_short": id8(cid),
                    "reason": reason.as_str(),
                }));
                left_boardable.push(id8(cid));
                self.merge_job_metadata(cid, vec![("skip_reason", json!(reason))])
                    .await?;
            }
            // The train's own record of what it refused and why. The
            // `empty` marker is what fires the `cancelled` terminal
            // (its predicate is collect.done AND metadata.empty) —
            // the same abandonment path an empty window takes, and
            // honest here: nothing boarded, because nothing could.
            self.merge_job_metadata(
                &train_id,
                vec![
                    ("empty", json!("true")),
                    (
                        "consist_check",
                        json!({
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
                            "cars_left_boardable": left_boardable,
                        }),
                    ),
                    ("left_behind", json!(left_behind)),
                ],
            )
            .await?;
            self.complete_step(
                &train,
                collect,
                &[("boarded", Some(format!("nothing — {reason}")))],
            )
            .await?;
            // The consist detail already rode onto the JOB above; this
            // puts the headline on the terminal, where the yard and
            // `boss orient` read a cancellation's reason. Twice on the
            // night of 2026-09-04 this refusal cancelled a train with
            // the terminal blank, and the only way to learn that one
            // lint could not EXECUTE (python3 absent from the
            // conductor's image) was to read the pod's log.
            self.record_abandon_reason(&train, &format!("consist check refused — {reason}"))
                .await?;
            log(format!(
                "consist check refused the consist: no PR opened, no CI spent — {} car(s) stay \
                 boardable and unstruck",
                boarded.len()
            ));
            return Ok(());
        }
        log(format!(
            "consist check: {} cheap lint(s) clean on the assembled tree",
            verdict.ran()
        ));

        sh(&["git", "-C", clone, "push", "fork", &train_branch])?;
        let train_ref_out = sh(&["git", "-C", clone, "rev-parse", "--short", "HEAD"])?;
        let train_ref = stdout_str(&train_ref_out).trim().to_string();

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
            // behind as "".
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
    match lock.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => {
            // A held lock means a conductor run is active right now — the
            // locomotive is demonstrably pulling; a standalone pre-flight
            // has nothing further to prove.
            log("another conductor run holds the lock — leaving");
            return Ok(());
        }
        Err(TryLockError::Error(e)) => {
            return Err(e).context("locking the conductor's lock file");
        }
    }
    let problems = preflight(&cfg)?;
    if !problems.is_empty() {
        for p in &problems {
            log(format!("preflight FAIL: {p}"));
        }
        // Exit 3 — distinct from a crash, loud in the unit's status.
        // (The lock releases with the process; destructors are moot.)
        std::process::exit(3);
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
            files: vec!["infra/platform/workflows.toml".into()],
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
        ApiFailure, ConvergenceVerdict, Failure, JOBS_API_RETRY, NO_PLAYGROUND_DEPLOY_EVIDENCE,
        RetryPolicy, SweepGuard, arrival_already_filed, arrival_report, arrival_summary,
        auto_cancel_reason, boarded_head, branch_moved_line, car_hold_reason, ci_overdue,
        classify_transport, commits_match, convergence_verdict, deletable_branches, deploy_needed,
        local_jobs_problem, merge_declined_reason, overlay_metadata, parked_ready,
        playground_deploy_disabled, releasable_cars, repo_path, resolve_train, retryable, retrying,
        short_cause, skip_reason_branch_missing, skip_reason_conflict, stall_age_hours,
        sweep_guard, sweep_note, sweep_settled, train_branch_to_delete, verdict_drift,
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

        // An EMPTY hold is not a hold. `truthy` treats "" as false, so
        // clearing the field releases the car without deleting the key.
        let mut released = ready_car();
        released["steps"][0]["metadata"] = json!({"hold": ""});
        assert!(parked_ready(&released));

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

    #[test]
    fn a_landed_cars_branch_is_deletable() {
        let cars = vec![landed_car("car-1", "feat/x")];
        assert_eq!(
            deletable_branches(&cars, &no_open()),
            vec![("feat/x".to_string(), "car-1".to_string())]
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
        assert_eq!(
            deletable_branches(&cars, &no_open()),
            vec![("feat/x".to_string(), "car-1".to_string())]
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

    /// A conductor whose config is literals and whose forge is the
    /// recorder — the cleanup touches neither the jobs API nor the
    /// tree, so nothing else needs to exist.
    fn cleanup_conductor(forge_kind: &str, forge: Box<dyn Forge>) -> Conductor {
        Conductor {
            cfg: Config {
                jobs: "http://jobs.invalid".into(),
                gh_repo: "example/boss".into(),
                head_owner: "example".into(),
                fork_url: "https://github.com/example/boss-fork.git".into(),
                upstream_url: "https://github.com/example/boss.git".into(),
                home: "/tmp/boss-train-test".into(),
                clone: "/tmp/boss-train-test/repo".into(),
                deploy_tree: "/tmp/boss-train-test/tree".into(),
                forge_kind: forge_kind.into(),
                auto_merge: false,
                allow_local_jobs: true,
                ci_hours: 2,
                converge_alarm_mins: 30,
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
        // And the /tmp path the tree-backed deploy tests exercise.
        assert!(!playground_deploy_disabled("/tmp/boss-train-test/tree"));
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

    #[test]
    fn only_a_branch_that_still_exists_is_worth_narrating() {
        // The sweep's journal is an operator surface: a line earns its
        // place by naming something a human can act on. A branch that
        // is not on the forge is not that — nothing to delete, nothing
        // to rescue, no action available.
        assert_eq!(sweep_note(&SweepGuard::Gone, "fix/x", "car-1"), None);
        // Delete narrates at the call site, which knows whether it was
        // a dry run, a deletion, or a race.
        assert_eq!(sweep_note(&SweepGuard::Delete, "fix/x", "car-1"), None);
        // The two keep-and-tell cases: the branch exists and the sweep
        // declined it, which is exactly what an operator must hear.
        let no_record = sweep_note(&SweepGuard::NoRecord, "fix/x", "car-1")
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
            "fix/conductor-hardening",
            "car-1",
        )
        .expect("a branch that outgrew its boarding is worth a line");
        assert_eq!(
            moved,
            branch_moved_line("fix/conductor-hardening", BOARDED, MOVED)
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
            branch_moved_line("fix/conductor-hardening", BOARDED, MOVED),
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
        let root = std::env::temp_dir().join(format!("boss-pcb-{name}"));
        let _ = std::fs::remove_dir_all(&root);
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
}

#[cfg(test)]
mod track_tests {
    use super::track_occupied_by;
    use serde_json::json;

    #[test]
    fn an_open_train_occupies_the_track_and_is_named() {
        let open = vec![json!({
            "id": "48b67f2e-9970-456f-817a-d085975b915f",
            "title": "PR train 2026-09-05 07:26",
            "status": "open"
        })];
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
}
