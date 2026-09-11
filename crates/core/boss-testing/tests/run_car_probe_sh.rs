//! `infra/forge/run-car-probe.sh` runs a car's recorded probe on the
//! FORGE HOST. The probe was written on the dev pod, which is a
//! different machine with different tools — so a probe can be correct
//! and unrunnable, and this test pins the mechanism that tells those
//! two apart.
//!
//! THE DEFECT (backlog f9304366, measured 2026-09-09). The auto-proof
//! loop's first live run: a train arrived, the rule filed one
//! ops-request per probed car, the forge's ops-runner answered both,
//! and NEITHER car proved. Both probes were
//! `kubectl -n boss-dev exec deploy/boss-conductor -- …`, which is
//! right from the pod and impossible from the forge (outside the
//! cluster, no kubeconfig). The recorded evidence was
//! `{"exit": 1, "output": ""}` — an exit code with empty streams, which
//! reads exactly like a false claim.
//!
//! The reason the streams were empty is the whole lesson: the probes
//! swallowed their own diagnostics. `kubectl … 2>&1 | grep -q …` sends
//! the shell's own `kubectl: command not found` INTO the pipe, where
//! grep eats it. Nothing run-car-probe.sh does with stderr can recover
//! a message the probe redirected away from it.
//!
//! So the script hands the probe's shell a `command_not_found_handle`
//! and a file descriptor the probe's redirections cannot reach (fd 9,
//! opened before the probe's own text runs). Every command bash could
//! not find is recorded there, and the attempt says `unrunnable` with
//! the tool named — CLAUDE.md §Diagnosis, a verdict must name what
//! failed.
//!
//! This test RUNS that prelude, extracted verbatim from the script, so
//! the mechanism cannot rot into a comment. It needs bash and nothing
//! else — the script's own jq/curl path is covered by the record-shape
//! pin in boss-cli's prove.rs.
//!
//! The second half of the file covers the OTHER way a probe can be
//! correct and worthless: reading the system of record unidentified, so
//! an absence assertion passes against a page it was never allowed to
//! see (61085a9e). Both are the same family — a probe whose evidence
//! cannot be told apart from a false claim.

use std::path::PathBuf;
use std::process::Command;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .expect("repo root resolves")
}

const BEGIN: &str = "# PROBE-PRELUDE-BEGIN";
const END: &str = "# PROBE-PRELUDE-END";

/// The prelude the script wraps every probe in, lifted from the script
/// itself between its two markers. One definition, executed here.
fn probe_prelude() -> String {
    let sh = std::fs::read_to_string(repo_root().join("infra/forge/run-car-probe.sh"))
        .expect("run-car-probe.sh is readable");
    let after = sh
        .split_once(BEGIN)
        .unwrap_or_else(|| panic!("run-car-probe.sh has no {BEGIN} marker"))
        .1;
    let body = after
        .split_once(END)
        .unwrap_or_else(|| panic!("run-car-probe.sh has no {END} marker"))
        .0;
    assert!(
        body.contains("command_not_found_handle"),
        "the extracted prelude does not install a command_not_found_handle:\n{body}"
    );
    body.to_string()
}

fn scratch(case: &str) -> PathBuf {
    // Per-uid and per-process, and it REFUSES by name if a
    // leftover cannot be cleared — see `boss_testing::scratch`.
    boss_testing::scratch_dir(&format!("run-car-probe-sh-{case}"))
}

/// Run one probe exactly as the script runs it: the prelude, then the
/// probe's own text, in one `bash -c`, with the not-found file named in
/// the environment. Returns (exit code, stdout, stderr, not-found log).
fn run_probe(case: &str, probe: &str) -> (i32, String, String, String) {
    let dir = scratch(case);
    let notfound = dir.join("notfound");
    let out = Command::new("bash")
        .arg("-c")
        .arg(format!("{}{probe}", probe_prelude()))
        .env("BOSS_PROBE_NOTFOUND", &notfound)
        .current_dir(&dir)
        .output()
        .expect("bash runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
        std::fs::read_to_string(&notfound).unwrap_or_default(),
    )
}

/// THE MEASURED CASE, verbatim in shape: a probe that pipes its own
/// stderr into a grep. The tool is missing, the streams the script
/// captures are empty — and the not-found channel still names it.
#[test]
fn a_missing_tool_is_named_even_when_the_probe_swallows_its_own_stderr() {
    let (rc, stdout, stderr, notfound) = run_probe(
        "swallowed",
        "kubectl-no-such-tool -n boss-dev get pods 2>&1 | grep -q Running \
         && echo PROBE_OK || echo PROBE_MISSING",
    );
    // What the script would have recorded before: exit + streams that
    // say nothing about why.
    assert_eq!(rc, 0, "the probe's own `|| echo` masks the exit code");
    assert!(stdout.contains("PROBE_MISSING"), "stdout: {stdout}");
    assert!(
        !stderr.contains("kubectl-no-such-tool"),
        "the probe redirected its diagnostics into the pipe; stderr: {stderr}"
    );
    // What it records now: the tool, on a channel the probe cannot
    // redirect.
    assert!(
        notfound.contains("kubectl-no-such-tool"),
        "the not-found channel must name the missing tool, got: {notfound:?}"
    );
}

/// The other live shape: an `&&` chain whose first link is missing, so
/// the probe exits 1 with nothing on either stream. Exit 1 and silence
/// is indistinguishable from a false claim — unless the tool is named.
#[test]
fn an_and_chain_that_dies_on_a_missing_tool_still_names_it() {
    let (rc, stdout, stderr, notfound) = run_probe(
        "and-chain",
        "kubectl-no-such-tool get cm 2>&1 | grep -q x && echo PROBE_OK",
    );
    assert_eq!(rc, 1, "the chain stops at the missing link");
    assert!(stdout.is_empty(), "stdout: {stdout}");
    assert!(stderr.is_empty(), "stderr: {stderr}");
    assert!(
        notfound.contains("kubectl-no-such-tool"),
        "the not-found channel must name the missing tool, got: {notfound:?}"
    );
}

/// A probe that does NOT redirect keeps bash's own message on stderr —
/// the prelude adds a channel, it does not take one away.
#[test]
fn the_prelude_still_prints_command_not_found_on_stderr() {
    let (_, _, stderr, notfound) = run_probe("plain", "kubectl-no-such-tool version");
    assert!(
        stderr.contains("kubectl-no-such-tool") && stderr.contains("command not found"),
        "stderr must still carry the shell's message, got: {stderr:?}"
    );
    assert!(notfound.contains("kubectl-no-such-tool"), "{notfound:?}");
}

/// A probe whose tools all exist records NOTHING on the not-found
/// channel — so `unrunnable` cannot be stamped on a claim that simply
/// turned out to be false.
#[test]
fn a_runnable_probe_leaves_the_not_found_channel_empty() {
    let (rc, stdout, _, notfound) = run_probe("runnable", "echo NOT_THE_WORD | grep -q TOKEN");
    assert_eq!(rc, 1, "the probe ran and its assertion failed");
    assert!(stdout.is_empty(), "{stdout}");
    assert!(
        notfound.trim().is_empty(),
        "a false claim must not look unrunnable, got: {notfound:?}"
    );
}

/// The prelude must not be able to take the probe down with it: with
/// no writable not-found file the probe still runs and still reports.
#[test]
fn an_unwritable_not_found_channel_does_not_break_the_probe() {
    let dir = scratch("unwritable");
    let out = Command::new("bash")
        .arg("-c")
        .arg(format!("{}echo PROBE_TOKEN_OK", probe_prelude()))
        .env("BOSS_PROBE_NOTFOUND", "/proc/nonexistent/dir/notfound")
        .current_dir(&dir)
        .output()
        .expect("bash runs");
    assert!(out.status.success(), "the probe must still run");
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("PROBE_TOKEN_OK"),
        "stdout: {}",
        String::from_utf8_lossy(&out.stdout)
    );
}

// =====================================================================
// A PROBE READS THE SYSTEM OF RECORD AS A NAMED READER (61085a9e).
//
// THE DEFECT, measured 2026-09-10 against one backend at one commit.
// The script builds the right `x-boss-user` header for its OWN three
// calls — read the car, write the verdict, patch the attempt — and
// hands the probe an env carrying only `BOSS_JOBS_URL`. So a probe
// doing `curl $BOSS_JOBS_URL/api/...` reads as `operator:unidentified`,
// and policy answers a NARROWER WORLD without saying so:
//
//   /api/jobs?kind=pr-train&status=open        operator 1  → 0
//   /api/jobs?kind=ship-a-change&status=open           21  → 0
//   /api/jobs?kind=backlog-item&status=open            30  → 0
//   /api/yard/status  trains/dock/recent/dock_depth 1/1/8/1 → all 0
//   /api/workflows                                     84  → 84 (same)
//
// Per-surface, with nothing in the answer to say which kind you hit.
// `/api/yard/status` comes back a confident, well-formed, completely
// idle yard: no trains, nothing on the dock, threshold not met.
//
// WHY THAT IS WORSE THAN A BROKEN READ. A PRESENCE assertion fails for
// the wrong reason and reads as a false negative about the car —
// recoverable, because someone investigates. An ABSENCE assertion
// PASSES FALSELY: "no open job of kind X remains" is green against an
// empty page the probe was never allowed to see. That is a proof that
// vouches for nothing, recorded as a proof, on a car that then closes.
// It caught the person who filed the item twice in one hour.
//
// THE FIX, and why it is not one line. Exporting the script's own
// `BOSS_USER` would hand every car's probe text a platform-admin
// credential with WRITE capability, and a probe is program text
// authored upstream and run as david on the forge. So the probe gets a
// SEPARATE, READ-SCOPED actor (`audit-readonly`: core policy grants it
// Read at Scope::All on every shipped resource and no other action at
// all) plus a GET-only reader on its PATH whose header comes from the
// runner, never from the probe's text.
// =====================================================================

const READER_BEGIN: &str = "# PROBE-READER-BEGIN";
const READER_END: &str = "# PROBE-READER-END";

fn script() -> String {
    std::fs::read_to_string(repo_root().join("infra/forge/run-car-probe.sh"))
        .expect("run-car-probe.sh is readable")
}

/// The read-scoped actor the runner builds for the probe, lifted from
/// between the script's markers. One definition, read here.
fn reader_actor_block() -> String {
    let sh = script();
    let after = sh
        .split_once(READER_BEGIN)
        .unwrap_or_else(|| panic!("run-car-probe.sh has no {READER_BEGIN} marker"))
        .1;
    after
        .split_once(READER_END)
        .unwrap_or_else(|| panic!("run-car-probe.sh has no {READER_END} marker"))
        .0
        .to_string()
}

/// The role the runner gives the probe's reader, READ OUT of the script
/// rather than asserted as a literal — so this test follows a rename
/// instead of going quiet on one.
fn reader_role() -> String {
    let block = reader_actor_block();
    let marker = "role";
    let start = block
        .find(marker)
        .unwrap_or_else(|| panic!("the reader actor names no role:\n{block}"))
        + marker.len();
    block[start..]
        .chars()
        .skip_while(|c| !c.is_ascii_alphanumeric())
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect()
}

/// The reader the probe is given, as the runner spells it on PATH.
const READER: &str = "boss-sor-read";

fn reader_path() -> PathBuf {
    repo_root().join("infra/forge/probe-bin").join(READER)
}

/// THE ENV THE PROBE GETS. It must carry a read-scoped actor, and it
/// must NOT carry the script's own platform-admin one: the probe runs
/// program text a builder wrote, and the privilege should match the job
/// — read the system of record, change nothing.
#[test]
fn the_probe_env_carries_a_read_scoped_actor_and_never_the_scripts_own() {
    let sh = script();
    // The two `env … bash -c "$probe_prelude$probe"` hand-offs: the
    // root path (runuser) and the by-hand path.
    let handoffs: Vec<&str> = sh
        .split("bash -c \"$probe_prelude$probe\"")
        .take(2)
        .collect();
    assert_eq!(
        handoffs.len(),
        2,
        "expected the two probe hand-offs (root + by-hand) in run-car-probe.sh"
    );
    for (which, segment) in ["root/runuser", "by-hand"].iter().zip(handoffs) {
        // Only the tail of each segment is the env list; take the last
        // 600 bytes so the header's prose cannot satisfy the assertion.
        let tail = &segment[segment.len().saturating_sub(600)..];
        assert!(
            tail.contains("BOSS_SOR_USER=\"$READER_USER\""),
            "the {which} hand-off does not export the read-scoped actor:\n{tail}"
        );
        assert!(
            !tail.contains(" BOSS_USER="),
            "the {which} hand-off exports the script's own platform-admin actor to the \
             probe — that is a WRITE credential handed to probe text:\n{tail}"
        );
        assert!(
            tail.contains("$PROBE_BIN"),
            "the {which} hand-off does not put the sanctioned reader on the probe's PATH:\n{tail}"
        );
    }
}

/// THE FACT THAT LIVES TWICE (CLAUDE.md §9a). The role is written in a
/// shell script and its meaning is defined in Rust; they cannot be
/// collapsed, so they are pinned equal. `audit-readonly` must grant
/// Read at Scope::All on what a probe reads — and no non-Read action
/// anywhere, which is what makes handing it to probe text safe.
#[test]
fn the_probes_reader_role_can_read_everything_and_write_nothing() {
    use boss_policy_client::{Action, Resource, Scope};

    let role = reader_role();
    let rules = boss_policy_client::defaults::default_rules();
    let mine: Vec<_> = rules.iter().filter(|r| r.role == role).collect();
    assert!(
        !mine.is_empty(),
        "run-car-probe.sh gives the probe role '{role}', which core policy does not seed at \
         all — an unseeded role reads NOTHING, which is the defect with extra steps"
    );
    for resource in [Resource::job(), Resource::step(), Resource::event()] {
        assert!(
            mine.iter().any(|r| r.resource == resource
                && r.action == Action::Read
                && r.scope == Scope::All),
            "'{role}' has no Read/All on {resource:?} — a probe carrying it would see a \
             narrower world than the operator, which is what 61085a9e measured"
        );
    }
    for rule in &mine {
        assert_eq!(
            rule.action,
            Action::Read,
            "'{role}' carries a non-Read grant ({:?} on {:?}) — it is handed to program text \
             a builder wrote, so it must not be able to change anything",
            rule.action,
            rule.resource
        );
    }
}

/// The reader the gate advises must EXIST, and be executable in the
/// checkout the forge runs probes from. A refusal that names a tool
/// nobody shipped is worse than no refusal.
#[test]
fn the_sanctioned_reader_ships_in_the_checkout_and_is_executable() {
    let p = reader_path();
    assert!(
        p.is_file(),
        "the probe's PATH points at {}, which does not exist",
        p.display()
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&p)
            .expect("reader metadata")
            .permissions()
            .mode();
        assert_eq!(
            mode & 0o111,
            0o111,
            "{} is not executable (mode {mode:o}) — the probe would get `command not found`",
            p.display()
        );
    }
}

/// Run the reader with a STUB `curl` first on PATH that records its
/// argv. Returns (exit code, stdout, stderr, argv one per line).
fn run_reader(case: &str, args: &[&str], env: &[(&str, &str)]) -> (i32, String, String, String) {
    let dir = scratch(case);
    let bin = dir.join("bin");
    boss_testing::create_dir(&bin);
    let log = dir.join("curl-argv");
    boss_testing::write_file(
        &bin.join("curl"),
        &format!(
            "#!/usr/bin/env bash\nfor a in \"$@\"; do printf '%s\\n' \"$a\"; done > '{}'\n\
             printf 'STUB_BODY\\n'\n",
            log.display()
        ),
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(bin.join("curl"), std::fs::Permissions::from_mode(0o755))
            .expect("chmod stub curl");
    }
    let mut cmd = Command::new(reader_path());
    cmd.args(args)
        .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
        .current_dir(&dir);
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("the reader runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
        std::fs::read_to_string(&log).unwrap_or_default(),
    )
}

/// THE READER IDENTIFIES THE PROBE. The header comes from the runner's
/// env, never from the probe's text, and the method is GET because
/// there is no method argument to give.
#[test]
fn the_reader_sends_the_runners_read_scoped_header_on_a_get() {
    let actor = format!("{{\"id\":\"automation:x\",\"role\":\"{}\"}}", reader_role());
    let (rc, stdout, stderr, argv) = run_reader(
        "reader-get",
        &["/api/yard/status"],
        &[
            ("BOSS_JOBS_URL", "http://sor.invalid:7900"),
            ("BOSS_SOR_USER", &actor),
        ],
    );
    assert_eq!(rc, 0, "stderr: {stderr}");
    assert!(stdout.contains("STUB_BODY"), "stdout: {stdout}");
    assert!(
        argv.contains(&format!("x-boss-user: {actor}")),
        "the reader did not send the runner's actor; argv:\n{argv}"
    );
    assert!(
        argv.contains("http://sor.invalid:7900/api/yard/status"),
        "the reader must read the base the runner pinned; argv:\n{argv}"
    );
    assert!(
        !argv.lines().any(|l| l == "-X"),
        "the reader must be GET-only — no method is selectable; argv:\n{argv}"
    );
}

/// AND IT REFUSES RATHER THAN READING UNIDENTIFIED. With no actor in
/// the env the reader must stop — never fall back to a bare read, which
/// is the false-absence shape this whole block is about. `curl` must not
/// even be reached.
#[test]
fn the_reader_refuses_rather_than_reading_unidentified() {
    let (rc, stdout, stderr, argv) = run_reader(
        "reader-no-actor",
        &["/api/yard/status"],
        &[("BOSS_JOBS_URL", "http://sor.invalid:7900")],
    );
    assert_ne!(
        rc, 0,
        "a reader with no identity must refuse; stdout: {stdout}"
    );
    assert!(
        stderr.contains("BOSS_SOR_USER"),
        "the refusal must name what is missing; stderr: {stderr}"
    );
    assert!(
        argv.is_empty(),
        "curl was invoked anyway — the unidentified read happened; argv:\n{argv}"
    );
}

/// A METHOD IS NOT SELECTABLE, AND NEITHER IS THE HOST. Both are the
/// "a wrong target answers instead of erroring" lesson: a probe that
/// could name its own URL could read the OTHER, older stack at boss-gcp
/// and get a well-formed answer about different data.
#[test]
fn the_reader_takes_a_path_and_refuses_anything_else() {
    let actor = "{\"id\":\"automation:x\",\"role\":\"audit-readonly\"}";
    for (case, args) in [
        (
            "reader-absolute-url",
            vec!["http://127.0.0.1:7900/api/jobs"],
        ),
        ("reader-method", vec!["PUT", "/api/jobs"]),
        ("reader-no-args", vec![]),
    ] {
        let (rc, _, stderr, argv) = run_reader(
            case,
            &args,
            &[
                ("BOSS_JOBS_URL", "http://sor.invalid:7900"),
                ("BOSS_SOR_USER", actor),
            ],
        );
        assert_ne!(rc, 0, "{case} must be refused");
        assert!(argv.is_empty(), "{case} reached curl anyway; argv:\n{argv}");
        assert!(!stderr.is_empty(), "{case} must say why");
    }
}
