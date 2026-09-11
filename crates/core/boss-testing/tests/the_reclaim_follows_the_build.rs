//! THE RECLAIM FOLLOWS THE BUILD.
//!
//! Measured on the forge host, 2026-09-11 (backlog `0357e0eb`): six
//! consecutive observations fifteen minutes apart read 89, 100, 99,
//! **60**, 92, 98 GB free of 227. The locomotive refuses to START a CI
//! run below 70 GB (`infra/forge/locomotive.sh`, `BOSS_CI_MIN_FREE_GB`),
//! so the 04:03 trough sat ten gigabytes under the door a train boards
//! through — and a locomotive refusal happens BEFORE any check runs, so
//! it says nothing about the branch while striking every car aboard.
//! Four clean cars lost five departures that way on 2026-08-22 and a
//! whole day went to holding on 2026-09-05.
//!
//! THE CAUSE IS A CADENCE MISMATCH, NOT A SHORTAGE, and both floors are
//! individually right. The FILL is event-driven: every CI run builds and
//! pulls a per-train `boss-ci:<sha>` image into the system docker
//! daemon. The RECLAIM is timer-driven: `disk-floor-sweep.timer` runs
//! hourly and its unit defends 100 GB — deliberately higher than the
//! locomotive's 70, so a floor buys headroom above the one being
//! defended. A dip whose amplitude exceeds that 30 GB gap, inside one
//! timer interval, walks straight through it.
//!
//! So the reclaim must follow the EVENT that fills the disk. The
//! mechanism was already complete and only the trigger was missing: the
//! forge runs an ops-runner on a ~1-minute poll, and `reclaim-disk` is
//! an allowlisted bounded verb (`infra/ops/verbs.json`) that runs the
//! SAME `disk-floor-sweep.sh` the timer runs. `request-reclaim-disk.sh`
//! is the trigger, and the CI workflow — which already runs on every
//! train, green or red, and needs no install step on the host — is what
//! fires it.
//!
//! What is pinned here:
//!  - the workflow actually asks, after the heavy jobs, whatever their
//!    verdict (a red train's CI filled the disk just the same);
//!  - the request is the allowlisted verb with the allowlisted shape;
//!  - the floor it asks for is the floor the sweep's own unit defends,
//!    read out of that unit rather than copied beside it (CLAUDE.md §9a
//!    — one number wearing two names is how the sweep and the
//!    locomotive drifted before);
//!  - nothing about this can fail a CI run. An accelerator that reds a
//!    train would cause the exact strike it exists to prevent, and the
//!    hourly timer remains the independent floor.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .expect("repo root resolves")
}

fn read(rel: &str) -> String {
    let path = repo_root().join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

const SCRIPT: &str = "infra/forge/request-reclaim-disk.sh";
const SWEEP_UNIT: &str = "infra/forge/disk-floor-sweep.service";
const WORKFLOW: &str = ".forgejo/workflows/ci.yml";

/// Every fixture path carries `{pid}`: the gate runs as uid 65534 and
/// several of these suites share one machine, so a fixed temp path is a
/// collision waiting for a parallel run.
fn fixture_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("boss-reclaim-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir fixture");
    dir
}

fn write_exec(path: &Path, body: &str) {
    std::fs::write(path, body).expect("write stub");
    std::fs::set_permissions(
        path,
        <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o755),
    )
    .expect("chmod stub");
}

/// A stub `df` reporting a fixed number of free GB in POSIX columns, and
/// a stub `curl` that records every invocation and answers with a
/// plausible `{"id": ...}`. Driving the real script beside stubs is the
/// idiom `infra/lint/ci-images-are-pruned-by-age.sh` already uses for
/// the prune loop: no daemon, no network, no system of record.
struct Stubs {
    dir: PathBuf,
    df: PathBuf,
    curl: PathBuf,
}

fn stubs(name: &str, free_gb: u64, curl_exit: i32) -> Stubs {
    let dir = fixture_dir(name);
    let df = dir.join("df");
    write_exec(
        &df,
        &format!(
            "#!/usr/bin/env bash\n\
             echo 'Filesystem 1024-blocks Used Available Capacity Mounted on'\n\
             echo \"/dev/fake 1 1 {} 50% /\"\n",
            free_gb * 1024 * 1024
        ),
    );
    let curl = dir.join("curl");
    write_exec(
        &curl,
        &format!(
            "#!/usr/bin/env bash\n\
             printf '%s\\n' \"$*\" >> {log}\n\
             echo '{{\"id\":\"11111111-2222-3333-4444-555555555555\"}}'\n\
             exit {exit}\n",
            log = dir.join("calls").display(),
            exit = curl_exit
        ),
    );
    Stubs { dir, df, curl }
}

impl Stubs {
    fn run(&self, jobs_url: Option<&str>) -> Output {
        let mut cmd = Command::new("bash");
        cmd.arg(repo_root().join(SCRIPT))
            .env("BOSS_RECLAIM_DF_CMD", &self.df)
            .env("BOSS_RECLAIM_CURL_CMD", &self.curl)
            .env_remove("BOSS_JOBS_URL")
            .current_dir(repo_root());
        if let Some(url) = jobs_url {
            cmd.env("BOSS_JOBS_URL", url);
        }
        cmd.output().unwrap_or_else(|e| {
            panic!(
                "run {}: {e} — the trigger does not exist yet, which is the point of this test",
                SCRIPT
            )
        })
    }

    fn calls(&self) -> String {
        std::fs::read_to_string(self.dir.join("calls")).unwrap_or_default()
    }
}

fn say(out: &Output) -> String {
    format!(
        "exit {:?}\nstdout: {}\nstderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// The floor the sweep's own unit defends — the ONE definition the
/// trigger must derive its request from rather than carry a copy of.
fn defended_floor() -> String {
    let unit = read(SWEEP_UNIT);
    unit.lines()
        .find_map(|l| l.trim().strip_prefix("Environment=BOSS_DISK_FLOOR_GB="))
        .map(|v| v.trim().to_string())
        .unwrap_or_else(|| {
            panic!(
                "{SWEEP_UNIT} no longer pins Environment=BOSS_DISK_FLOOR_GB — \
                 the trigger reads its floor from there, so nothing can ask for \
                 the floor the sweep actually defends"
            )
        })
}

// ---------------------------------------------------------------------
// The wiring: the workflow asks, after the heavy jobs, whatever happened
// ---------------------------------------------------------------------

/// The job that fires the trigger. `test` is the last of the heavy jobs
/// and `reclaim` follows it, so the slice runs to the end of the file.
fn reclaim_job() -> String {
    let ci = read(WORKFLOW);
    let start = ci.find("\n  reclaim:").unwrap_or_else(|| {
        panic!(
            "{WORKFLOW} has no `reclaim` job — a CI run still fills the forge's \
             disk with a per-train image and asks nobody to reclaim it, so the \
             only reclaim is the hourly timer and a 30GB dip inside one interval \
             walks through the locomotive's floor (backlog 0357e0eb)"
        )
    });
    ci[start + 1..].to_string()
}

#[test]
fn the_workflow_asks_for_a_reclaim_when_a_ci_run_completes() {
    let job = reclaim_job();
    assert!(
        job.contains(SCRIPT),
        "the `reclaim` job does not invoke {SCRIPT} — the trigger is what makes \
         the reclaim follow the build instead of the clock"
    );
}

#[test]
fn the_reclaim_waits_for_the_heavy_jobs_and_runs_whatever_their_verdict() {
    let job = reclaim_job();
    let needs = job
        .lines()
        .find(|l| l.trim_start().starts_with("needs:"))
        .unwrap_or_else(|| panic!("the `reclaim` job declares no `needs:`"))
        .to_string();
    for heavy in ["fast", "test", "web"] {
        assert!(
            needs.contains(heavy),
            "the `reclaim` job does not wait for `{heavy}` ({needs}) — reclaiming \
             while a job is still building frees nothing it is allowed to touch, \
             because the space in flight is a live workspace volume"
        );
    }
    assert!(
        job.contains("always()"),
        "the `reclaim` job is not guarded with always() — a RED train's CI built \
         and pulled the same per-train image, so skipping the reclaim on failure \
         drops the reclaim exactly when the disk is worst. It must also survive a \
         locomotive refusal, which is the self-healing case: the door refused for \
         want of disk, and this is what frees it before the next boarding."
    );
    assert!(
        job.contains("continue-on-error: true"),
        "the `reclaim` job is not continue-on-error — an accelerator that can red \
         a train causes the strike it exists to prevent"
    );
}

// ---------------------------------------------------------------------
// The request: the allowlisted verb, with the floor the sweep defends
// ---------------------------------------------------------------------

#[test]
fn below_the_floor_it_files_an_ops_request_for_the_reclaim_disk_verb() {
    let s = stubs("below-floor", 60, 0);
    let out = s.run(Some("http://10.20.0.34:7900"));
    assert!(
        out.status.success(),
        "the trigger must never fail a CI run.\n{}",
        say(&out)
    );
    let calls = s.calls();
    assert!(
        calls.contains("/api/jobs"),
        "60GB free is below the sweep's floor and no packet was filed.\ncalls: \
         {calls}\n{}",
        say(&out)
    );
    for needle in [
        "\"kind\":\"ops-request\"",
        "\"host\":\"forge\"",
        "\"verb\":\"reclaim-disk\"",
    ] {
        assert!(
            calls.contains(needle),
            "the filed packet does not carry {needle} — the ops-runner matches \
             metadata.host exactly and reads the verb out of the allowlist, so a \
             packet missing either sits open and unanswered.\ncalls: {calls}"
        );
    }
    assert!(
        !calls.contains("127.0.0.1"),
        "the packet went somewhere other than the system of record it was given \
         — a wrong target answers instead of erroring.\ncalls: {calls}"
    );
}

#[test]
fn the_floor_it_asks_for_is_the_floor_the_sweep_defends() {
    let floor = defended_floor();
    let s = stubs("floor-number", 10, 0);
    let out = s.run(Some("http://10.20.0.34:7900"));
    let calls = s.calls();
    assert!(
        calls.contains(&format!("\"args\":[\"{floor}\"]")),
        "the trigger asked for a floor other than the {floor}GB \
         {SWEEP_UNIT} defends. The reclaim-disk verb's own default is 25, which \
         defends nothing in the band where CI refuses — and a copy of the number \
         beside the unit is the §9a pair that already drifted once between the \
         sweep and the locomotive. Read it from the unit.\ncalls: {calls}\n{}",
        say(&out)
    );
}

#[test]
fn above_the_floor_it_files_nothing() {
    let s = stubs("above-floor", 180, 0);
    let out = s.run(Some("http://10.20.0.34:7900"));
    assert!(
        out.status.success(),
        "a healthy host must be a quiet exit.\n{}",
        say(&out)
    );
    assert!(
        s.calls().is_empty(),
        "180GB free is above the sweep's floor: the sweep itself would log \
         `nothing to do`, so filing a packet per CI run would put tens of \
         auto-answered packets a day on the board for no work.\ncalls: {}",
        s.calls()
    );
}

// ---------------------------------------------------------------------
// It can never fail the run it rides in
// ---------------------------------------------------------------------

#[test]
fn an_unreachable_system_of_record_never_fails_the_run() {
    // curl exit 7 — connection refused.
    let s = stubs("unreachable", 60, 7);
    let out = s.run(Some("http://10.20.0.34:7900"));
    assert!(
        out.status.success(),
        "an unreachable system of record must cost this run its ACCELERATOR and \
         nothing else: the hourly disk-floor-sweep.timer owes nothing to the SoR \
         and remains the independent floor, so a best-effort request that fails \
         costs latency, never a missed reclaim. Exiting non-zero here would red a \
         train for a packet nobody could file.\n{}",
        say(&out)
    );
    let both = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        both.contains("hourly") || both.contains("timer"),
        "a failed request must say what still covers the disk, or the next reader \
         has to re-derive it.\n{}",
        say(&out)
    );
}

#[test]
fn it_refuses_to_guess_the_system_of_record() {
    let s = stubs("no-url", 60, 0);
    let out = s.run(None);
    assert!(
        out.status.success(),
        "a missing BOSS_JOBS_URL is a configuration fault, and in a CI job a \
         non-zero exit for one strikes the cars aboard.\n{}",
        say(&out)
    );
    assert!(
        s.calls().is_empty(),
        "with no system of record named the trigger guessed one. Defaulting to \
         127.0.0.1 is how weeks of maintenance packets landed on a \
         non-authoritative instance (2026-08-17).\ncalls: {}",
        s.calls()
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("BOSS_JOBS_URL"),
        "the refusal does not name what is missing.\n{}",
        say(&out)
    );
}
