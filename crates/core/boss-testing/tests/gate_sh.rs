//! The rust gate has ONE definition: `infra/gate.sh`.
//!
//! On the 2026-08-10 train (PR #226) the gate's definition lived twice —
//! once in `.github/workflows/ci.yml`, once in whatever the agent ran
//! locally before pushing a car — and drifted twice in one day: a car
//! gated with named test files missed a lib-suite pin, and a car gated
//! with full crate suites missed a shell lint only CI ran. CLAUDE.md
//! §9a: collapse the pair, and pin what cannot collapse.
//!
//! The collapse: the CI workflow's test job invokes `infra/gate.sh`
//! instead of inlining cargo commands and lint scripts, so CI and a
//! local run are the same definition. What cannot collapse is pinned
//! here:
//! - the workflow must actually call the script, and must not grow a
//!   second inline definition beside it (a new `run: infra/lint/...`
//!   line in the test job is the pair reopening);
//! - the script must keep covering the checks the gate exists to run —
//!   a trimmed roster is exactly the under-covering gate that let both
//!   #226 failures through.
//!
//! There is ONE CI workflow now: `.forgejo/workflows/ci.yml`. The GitHub
//! copy (`.github/workflows/ci.yml`) ran only on the public mirror —
//! which is a backup of source, not part of CI/CD (design 7b59af2c,
//! 2026-09-08) — and was deleted with it, so the pair it once formed
//! with the forge file is gone rather than pinned.
//!
//! Every test names the offending entry when it fails.

use std::path::PathBuf;

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

/// The `test`-job slice of the Forgejo workflow — the job that carries
/// the Postgres service, and so the only one that can run the gate's
/// DB-backed test phase. `test` is the last job in the file, so the
/// slice runs to the end; a job appended after it would be swept in,
/// which only ever makes the no-second-definition check stricter.
fn forge_test_job() -> String {
    let ci = read(".forgejo/workflows/ci.yml");
    let start = ci
        .find("\n  test:")
        .expect(".forgejo/workflows/ci.yml has a test job");
    ci[start + 1..].to_string()
}

/// The forge workflow is the one that actually gates a train — since
/// the 2026-08-12 cutover every car lands through Forgejo. For a day it
/// ran locomotive + fmt + clippy + migrate + build + test and NOT the
/// script, so the whole lint roster was unenforced in production and
/// thirteen trains landed green over a real `no-wallclock` violation.
/// The pin at the time only knew about the GitHub file, which is why
/// nothing caught it.
#[test]
fn forge_test_job_invokes_the_gate_script() {
    let job = forge_test_job();
    assert!(
        job.contains("infra/gate.sh"),
        ".forgejo/workflows/ci.yml's test job does not invoke \
         infra/gate.sh — the workflow that gates every train has \
         forked away from the gate's definition"
    );
}

#[test]
fn forge_test_job_has_no_inline_second_definition() {
    let job = forge_test_job();
    // Environment setup (services, schema apply) stays in the
    // workflow, checks live in the script. The
    // `fast` job's fmt + clippy are deliberately outside this slice —
    // they are a duplicated fast-signal loop, not a second definition.
    let inline_checks = [
        "run: cargo clippy",
        "run: cargo test",
        "run: cargo build",
        "run: cargo fmt",
        "run: infra/lint/",
    ];
    for needle in inline_checks {
        assert!(
            !job.contains(needle),
            ".forgejo/workflows/ci.yml's test job inlines `{needle}` \
             beside infra/gate.sh — the gate now has two definitions \
             again; move the check into the script"
        );
    }
}

#[test]
fn gate_script_covers_the_checks() {
    let gate = read("infra/gate.sh");
    // The four cargo phases, with the flags that made each one catch a
    // real bug class (see ci.yml history for the provenance of each).
    let cargo_phases = [
        "cargo clippy",
        "-D warnings",
        "cargo build --workspace",
        "--all-features",
        "cargo fmt -- --check",
    ];
    for needle in cargo_phases.iter() {
        assert!(
            gate.contains(needle),
            "infra/gate.sh no longer runs `{needle}` — the gate \
             under-covers what it existed to cover"
        );
    }

    // The lint roster used to be hand-listed in gate.sh, and by
    // 2026-08-13 this test had drifted to a strict subset of it. It was
    // pinned then; on 2026-09-05 the list itself was collapsed — four
    // cars collided on its tail line in one day and train #218 left one
    // behind — so gate.sh now derives the pre-flight roster from
    // infra/lint/ minus a named exclusion set (CLAUDE.md §9a, the
    // manifest.txt lesson one level up).
    //
    // Under-covering can therefore arrive only one way now: a lint
    // slipping into that exclusion set. So the exclusion set is what is
    // pinned. It is asked of the script itself (`--roster`), not
    // re-parsed from its text — a second parser of the array would be
    // the pair reopening.
    let not_preflighted: &[(&str, &str)] = &[
        (
            "conservation-invariants.sh",
            "live-DB sweep on a systemd timer, not a static check",
        ),
        (
            "audit-ordering.sh",
            "live-DB sweep; needs a populated audit_log to say anything",
        ),
        (
            "no-snapshot-arrays.sh",
            "needs a built workspace (boss-ports-list) — gating it is \
             proposed separately; it is the check that would have caught \
             the stale _generated/ports.ts",
        ),
        (
            "svelte-check.sh",
            "installs packages — minutes, not seconds; the gate's web phase \
             runs it, and that is asserted below",
        ),
    ];

    let out = std::process::Command::new("bash")
        .arg(repo_root().join("infra/gate.sh"))
        .arg("--roster")
        .current_dir(repo_root())
        .output()
        .expect("run gate.sh --roster");
    assert!(
        out.status.success(),
        "infra/gate.sh --roster refused: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let roster = String::from_utf8_lossy(&out.stdout);
    let preflighted: Vec<&str> = roster
        .lines()
        .filter_map(|l| l.split_once(' ').map(|(_, path)| path))
        .collect();
    assert_eq!(
        preflighted.first().copied(),
        Some("infra/lint/workspace-declares-what-it-runs.sh"),
        "the pre-flight must open by saying what the workspace cannot cover"
    );

    let mut missing = Vec::new();
    for entry in std::fs::read_dir(repo_root().join("infra/lint")).expect("read infra/lint") {
        let path = entry.expect("dir entry").path();
        let name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) if n.ends_with(".sh") => n.to_string(),
            _ => continue,
        };
        let excluded = not_preflighted.iter().any(|(n, _)| *n == name);
        let runs = preflighted
            .iter()
            .any(|p| *p == format!("infra/lint/{name}"));
        if excluded == runs {
            missing.push(name);
        }
    }
    missing.sort();
    assert!(
        missing.is_empty(),
        "infra/lint/ and gate.sh's pre-flight disagree on: {missing:?}. A lint \
         listed here as not-preflighted must be in gate.sh's PREFLIGHT_EXCLUDES, \
         and one excluded there must be listed here with the reason it is exempt."
    );
    assert!(
        gate.contains("infra/lint/svelte-check.sh"),
        "svelte-check.sh is kept out of the pre-flight for cost, not for coverage: \
         the gate's web phase must still run it"
    );
}

/// THE GATE MUST NOT EAT THE DISK IT IS RUNNING ON.
///
/// Twice in two days a full `infra/gate.sh` on the Mac filled the
/// volume and took the whole session with it, not just the run
/// (packet `865992c1`). The second time is the instructive one: every
/// subsequent command failed *before executing*, because the agent
/// harness could not create the file it writes command output into
/// ("ENOSPC ... open '.../tasks/*.output'"), so `df` and `rm` were
/// equally unavailable and the failure had disabled its own diagnosis.
///
/// A one-shot precondition cannot catch this. The run STARTS with
/// plenty and then grows a `target/` — 32GB in the reported incident,
/// 81GB on this machine when the guard was written — so the check has
/// to be re-evaluated as the run proceeds. `check()` is where every
/// phase passes through, which makes it the poll point.
///
/// Driven through the real script with an impossible floor, because
/// the behaviour under test is "does the guard actually stop the run"
/// and a unit test of the arithmetic would not answer that.
#[test]
fn the_gate_refuses_to_run_without_headroom() {
    let out = std::process::Command::new("bash")
        .arg(repo_root().join("infra/gate.sh"))
        .arg("--auto")
        .env("BOSS_GATE_MIN_FREE_GB", "99999999")
        .current_dir(repo_root())
        .output()
        .expect("run gate.sh");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "a floor no disk can satisfy must fail the gate.\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stderr.contains("Refusing to start"),
        "the refusal must name itself so it is not read as a test failure — \
         that misreading is the whole packet.\nstderr: {stderr}"
    );
    assert!(
        stderr.contains("99999999"),
        "the refusal must state the floor it applied.\nstderr: {stderr}"
    );
    assert!(
        !stdout.contains("all checks green"),
        "an aborted gate must never report green.\nstdout: {stdout}"
    );
}

/// THE POLL, which is the half a startup check cannot cover.
///
/// A fake `df` that reports plenty once and then almost nothing models
/// the incident directly: the gate started with headroom and the run
/// itself consumed it. The assertion is that the gate notices at the
/// next phase boundary and stops — and that it says "to continue", so
/// a log reader can tell this from a machine that was too small to
/// begin with.
#[test]
fn the_gate_rechecks_headroom_as_the_run_proceeds() {
    let root = repo_root();
    let dir = boss_testing::scratch_dir("boss-gate-headroom-poll");
    let counter = dir.join("calls");
    let fake = dir.join("df");
    // 1st call: 900GB free. Every later call: 1GB.
    std::fs::write(
        &fake,
        format!(
            "#!/usr/bin/env bash\n\
             n=$(cat {c} 2>/dev/null || echo 0)\n\
             echo $((n+1)) > {c}\n\
             echo 'Filesystem 1024-blocks Used Available Capacity Mounted on'\n\
             if [ \"$n\" -eq 0 ]; then echo '/dev/fake 1 1 943718400 1% /'; \
             else echo '/dev/fake 1 1 1048576 99% /'; fi\n",
            c = counter.display()
        ),
    )
    .expect("write fake df");
    std::fs::set_permissions(
        &fake,
        <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o755),
    )
    .expect("chmod");

    let out = std::process::Command::new("bash")
        .arg(root.join("infra/gate.sh"))
        .arg("--auto")
        .env("BOSS_GATE_DF_CMD", fake.to_str().expect("utf8"))
        .env("BOSS_GATE_MIN_FREE_GB", "12")
        // THE POLL NEEDS THE GATE TO REACH A PHASE, and `--auto` only
        // reaches one if it derives a scope. Against the default trunk
        // that holds on a feature branch and NOT on main, where the
        // tree is clean and HEAD is its own trunk — so this test
        // passed everywhere except the one place it had to run, and
        // left main red after the startup half was fixed.
        //
        // `HEAD~1` always yields exactly the last commit's changes, on
        // a branch or on main, so the derivation succeeds in both and
        // the poll is tested rather than the scope.
        .env("BOSS_GATE_TRUNK", "HEAD~1")
        .current_dir(&root)
        .output()
        .expect("run gate.sh");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let _ = std::fs::remove_dir_all(&dir);

    assert!(
        stdout.contains("gate:") || !stdout.is_empty(),
        "the gate should have started — the first reading was 900GB.\nstderr: {stderr}"
    );
    assert!(
        !out.status.success(),
        "the run consumed its own headroom and must stop.\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stderr.contains("Refusing to continue"),
        "a mid-run trip must be distinguishable from a too-small machine.\nstderr: {stderr}"
    );
    assert!(
        !stdout.contains("all checks green"),
        "an aborted gate must never report green.\nstdout: {stdout}"
    );
}

/// THE HEADROOM CHECK MUST COME BEFORE SCOPE DERIVATION.
///
/// It landed after `--auto`'s derivation, which made the two tests
/// above depend on branch context: on a PR branch `--auto` finds
/// commits and reaches the disk check, but on a PUSH TO MAIN
/// `HEAD == origin/main` and the tree is clean, so `--auto` exits
/// first with "found no change at all" and the disk refusal never
/// runs. Both tests passed as PR #64 and then reddened main twice
/// (forge runs 155 and 157).
///
/// THIS IS A TEXT PIN, NOT A BEHAVIOURAL ONE, and deliberately so. I
/// wrote the behavioural version first and it could not fail: it
/// drives the real script from this working tree, and a working tree
/// with any edit in it always has changes for `--auto` to find, so
/// the no-change branch is unreachable from a test run. Reproducing
/// it needs a clean checkout whose HEAD is its own trunk — which is
/// CI, not a test. A test that cannot go red is worse than no test,
/// so this asserts the one thing that actually encodes the fix: the
/// order of two lines in the script.
#[test]
fn headroom_is_checked_before_the_scope_is_derived() {
    let script = std::fs::read_to_string(repo_root().join("infra/gate.sh")).expect("read gate.sh");
    let headroom = script
        .find("require_headroom \"to start\"")
        .expect("gate.sh still calls require_headroom at startup");
    let derivation = script
        .find("AUTO_TRUNK=")
        .expect("gate.sh still derives a trunk for --auto");
    assert!(
        headroom < derivation,
        "require_headroom must run BEFORE trunk derivation. After it, `--auto` \
         exits with 'found no change at all' on any clean checkout whose HEAD is \
         its trunk — every push to main — and the disk refusal never runs. \
         Refusing for want of disk should not wait on git archaeology either."
    );
}

/// The receipt must distinguish "the database-backed checks did not
/// run here" from "this change is fine".
///
/// Three red trains on 2026-08-18 came from cars whose local gate read
/// "26 of 28 — the two failures are the absent local Postgres". That
/// sentence was true every time and the car was broken every time:
/// migration ordering against a unique index, then a registry seed
/// disagreeing with its migration, then the same pin again. None is
/// visible to `bash -n` or to the shape lints, and the receipt gave no
/// way to tell an environmental failure from a real one — so the
/// author supplied the optimistic reading three times running.
///
/// `unverifiable` closes that: it lists the changed paths only a
/// database can judge, and it is empty unless the DB-backed checks
/// actually failed to pass.
#[test]
fn the_receipt_names_what_only_a_database_could_have_judged() {
    let script = read("infra/gate.sh");

    assert!(
        script.contains("db_backed_paths()"),
        "the gate must be able to name the paths a database judges"
    );
    assert!(
        script.contains("db_checks_passed()"),
        "the gate must know whether the DB-backed checks actually passed"
    );
    assert!(
        script.contains("\"unverifiable\": [${unver}]"),
        "the receipt must carry the `unverifiable` list — a consumer \
         reading only `verdict` and `checks` cannot tell an absent \
         database from a sound change"
    );

    // The classification itself. Schema and the dispatcher registry are
    // the two that actually bit; seed TOMLs are the same shape.
    let filter = script
        .lines()
        .find(|l| l.contains("changed_paths | grep -E '^infra/postgres/schema/"))
        .expect("db_backed_paths filters changed paths");
    for needle in [
        "infra/postgres/schema/",
        "infra/dispatcher/rules/",
        "/seeds/",
    ] {
        assert!(
            filter.contains(needle),
            "db_backed_paths must cover {needle} — it is DB-judged and has drifted before:\n{filter}"
        );
    }

    // Emptiness is load-bearing: if it listed paths whenever the DB
    // checks failed, every car on this Mac would carry the warning and
    // it would be ignored within a day.
    assert!(
        script.contains("if ! db_checks_passed; then"),
        "the list must be gated on the DB checks NOT passing, so it \
         stays silent on changes a database has nothing to say about"
    );
}

/// `--quick` must stop BEFORE anything compiles, or it is not quick.
///
/// The mode exists because the cheap checks were unreachable without the
/// expensive ones: on 2026-08-27 a car spent 17 minutes of cluster time,
/// a scheduled pod and a clone to discover a `cargo fmt` slip that
/// `--quick` now finds in 13 seconds. The property that makes it worth
/// running is that it does not build — so this asserts by POSITION,
/// which is the only thing that can actually go wrong here: move the
/// early exit below the cargo phases and `--quick` silently becomes a
/// full gate that lies about its name.
#[test]
fn quick_mode_exits_before_the_first_compile() {
    let gate = read("infra/gate.sh");

    // BY LINE, AND ONLY REAL INVOCATIONS. gate.sh is more comment than
    // code, and two earlier drafts of this test compared byte offsets
    // against `cargo build` and `check "fixture"` as they appear in
    // PROSE — 25k and 9k bytes above any real call. A needle that can
    // match a comment tests the comment. A `check` invocation is a line
    // whose first non-space characters are `check "`; a comment's are `#`.
    let lines: Vec<&str> = gate.lines().collect();
    let is_invocation = |l: &str| l.trim_start().starts_with("check \"");

    let quick_at = lines
        .iter()
        .position(|l| l.contains("if [ \"$QUICK\" -eq 1 ]; then"))
        .expect("infra/gate.sh no longer has a --quick early exit");

    // The three that COMPILE, named rather than matched on "cargo ".
    // `cargo fmt` is a cargo command that builds nothing and is part of
    // the pre-flight itself, so a broad needle finds it and reports the
    // mode failing to exit before a check it is supposed to run.
    let builds = |l: &str| {
        ["cargo clippy", "cargo build", "cargo test"]
            .iter()
            .any(|c| l.contains(c))
    };
    let first_compile = lines
        .iter()
        .enumerate()
        .find(|(_, l)| is_invocation(l) && builds(l))
        .map(|(i, l)| (i, l.trim().to_string()))
        .expect("gate.sh no longer compiles anything through check()");

    assert!(
        quick_at < first_compile.0,
        "`--quick` exits at line {} but the first compiling check is at line {} ({}) — \
         the early exit must come FIRST or --quick compiles, which is the one thing \
         it promises not to do",
        quick_at + 1,
        first_compile.0 + 1,
        first_compile.1
    );
}

/// The full gate must still run the pre-flight set.
///
/// `run_preflight` holds fmt plus the whole lint roster. If it were only
/// ever called from the `--quick` branch, a normal gate would stop
/// linting entirely and stay green while doing less — the exact
/// under-covering shape `gate_script_covers_the_checks` was written for,
/// one level up. So it has to be invoked somewhere the QUICK branch is
/// not.
#[test]
fn the_full_gate_still_runs_the_preflight_set() {
    let gate = read("infra/gate.sh");
    let calls = gate.matches("\nrun_preflight").count();
    assert!(
        calls >= 2,
        "`run_preflight` is invoked {calls} time(s); the full gate and --quick must \
         BOTH call it, or one of them silently skips fmt and every lint"
    );
}

/// No lint can truncate the roster, and the gate says so on every run.
///
/// Until 2026-09-10 `run_preflight` ran the roster as `while read -r
/// name path; do check "$name" bash "$path"; done <<< "$roster"`, which
/// handed every lint the REMAINING ROSTER LINES as its stdin. A lint
/// that reads stdin — a `grep` or `awk` whose file list came out empty,
/// a bare `cat`, a `read` — ate the rest of the roster; the loop ended
/// normally, and the gate printed `pre-flight: clean` and exited 0.
/// Measured on a draft lint: a 61-lint roster ran NINE checks and the
/// gate called it clean (backlog 9d5797d4). That is a false green on the
/// pipeline's lint authority, and the same under-covering shape as
/// `gate_script_covers_the_checks` — arriving through the loop instead
/// of through the list.
///
/// The shell pin is `gate.sh --self-test`, and it runs inside every mode
/// the gate has, so it cannot be true only when someone remembers to
/// ask. Two things are asserted here that the shell cannot assert about
/// itself:
///
/// - it HOLDS on this tree, run rather than read (a text assertion about
///   a redirection would pass on a comment);
/// - `run_preflight` still CALLS it. Delete that one line and the pin
///   goes quiet while staying green under `--self-test`, which is the
///   "check nobody reads" failure one level up.
#[test]
fn no_lint_can_truncate_the_roster() {
    let gate = read("infra/gate.sh");

    // The INVOCATION inside run_preflight, not the word anywhere in the
    // file: the function's own definition and this test's provenance
    // comment both name it.
    let body: String = gate
        .split_once("\nrun_preflight() {")
        .map(|(_, rest)| rest.split("\n}").next().unwrap_or("").to_string())
        .expect("infra/gate.sh defines run_preflight");
    assert!(
        body.lines()
            .any(|l| l.trim() == "roster_loop_self_test" || l.trim() == "roster_loop_self_test;"),
        "run_preflight no longer calls roster_loop_self_test — the pin on the roster loop \
         then runs only when someone types --self-test, and a lint that eats the roster is \
         back to printing `pre-flight: clean` (backlog 9d5797d4). run_preflight reads:\n{body}"
    );

    // And it passes. The three cases each fail when exactly one of the
    // three mechanisms is removed, verified by mutation when this landed:
    // revert the loop to stdin, drop the `< /dev/null` on the call, or
    // remove the count comparison, and one case names it.
    let out = std::process::Command::new("bash")
        .arg(repo_root().join("infra/gate.sh"))
        .arg("--self-test")
        .current_dir(repo_root())
        .output()
        .expect("run gate.sh --self-test");
    assert!(
        out.status.success(),
        "infra/gate.sh --self-test failed — the roster loop cannot be trusted to run every \
         lint, so no gate receipt from this tree says what it claims:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The pre-push hook exists, is executable, and actually runs the
/// pre-flight.
///
/// A hook that is documented but not installed is advice, and advice is
/// what failed: `--quick` existed on 2026-08-28 and a push still went out
/// with a formatting slip, because the pre-flight had been chained with
/// `;` instead of `&&`. The cost is a full gate — ~40 minutes of cluster
/// time and a scheduled pod — for something answerable in 11 seconds.
///
/// Asserting the file merely exists would pass on an empty one, so this
/// checks the three properties that make it a check rather than a
/// gesture: it is executable, it invokes the pre-flight, and it exits
/// non-zero when the pre-flight fails.
#[test]
fn the_pre_push_hook_runs_the_preflight_and_refuses_on_failure() {
    let path = repo_root().join("infra/git-hooks/pre-push");
    let hook = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path)
            .expect("stat the hook")
            .permissions()
            .mode();
        assert!(
            mode & 0o111 != 0,
            "infra/git-hooks/pre-push is not executable — git will ignore it silently"
        );
    }

    // Match the INVOCATION, not the prose. This asserted
    // `hook.contains("--quick")` anywhere in the file until 2026-08-28,
    // when the hook moved to `--lint` and the assertion kept passing on
    // the word "--quick" left behind in a comment. A test satisfied by
    // its own documentation is not testing anything.
    let invokes_preflight = hook.lines().any(|l| {
        let l = l.trim();
        !l.starts_with('#')
            && l.contains("gate.sh")
            && (l.contains("--quick") || l.contains("--lint"))
    });
    assert!(
        invokes_preflight,
        "no non-comment line invokes gate.sh with --quick or --lint — the hook is \
         a file that does nothing"
    );
    assert!(
        hook.contains("exit 1"),
        "the hook must REFUSE the push when the pre-flight fails; a hook that \
         only prints is the advice this replaces"
    );
    assert!(
        hook.contains("BOSS_SKIP_PREFLIGHT"),
        "there must be a deliberate escape hatch — a check with no way out \
         gets disabled wholesale the first time it is wrong"
    );
}

/// The install is one command and it has to be written down somewhere a
/// new clone will look, or the hook ships switched off.
#[test]
fn the_bootstrap_says_how_to_install_the_hook() {
    let doc = read("docs/runbooks/dev-environment-bootstrap.md");
    assert!(
        doc.contains("core.hooksPath") && doc.contains("infra/git-hooks"),
        "dev-environment-bootstrap.md does not say to set core.hooksPath — \
         a tracked hooks directory that nobody points git at is inert"
    );
}

/// EVERY CHECK CARRIES ITS DURATION, so a stall reads as a stall.
///
/// The receipt's `checks` array recorded `{name, result}` and nothing
/// else, which makes the two failure shapes that matter look identical:
/// a check that failed on the code, and a check that failed because it
/// was starved. A web unit test stalling ~8s under two parallel gates
/// once reddened a car that had nothing wrong with it; the receipt
/// could not say so, and the only way to learn it was to read a pod log
/// that no longer existed. A duration is one number per check and it is
/// the number that tells those two apart.
///
/// Driven through the real script — a text pin would assert that the
/// arithmetic was WRITTEN, not that the receipt CARRIES it. `--quick`
/// plus a fake `df` that goes tiny after the first check is the cheapest
/// path to a written receipt: nothing compiles, `fmt` runs, and the
/// mid-run headroom refusal writes the receipt with one real, timed
/// check in it.
#[test]
fn the_receipt_times_every_check() {
    let root = repo_root();
    let dir = boss_testing::scratch_dir("boss-gate-check-timing");
    let counter = dir.join("calls");
    let fake = dir.join("df");
    let receipt = dir.join("receipt.json");
    // Calls 1 (startup) and 2 (before `fmt`) see plenty; call 3 (before
    // the first lint) trips, which is what makes the gate write a
    // receipt holding exactly one, real, timed check.
    std::fs::write(
        &fake,
        format!(
            "#!/usr/bin/env bash\n\
             n=$(cat {c} 2>/dev/null || echo 0)\n\
             echo $((n+1)) > {c}\n\
             echo 'Filesystem 1024-blocks Used Available Capacity Mounted on'\n\
             if [ \"$n\" -lt 2 ]; then echo '/dev/fake 1 1 943718400 1% /'; \
             else echo '/dev/fake 1 1 1048576 99% /'; fi\n",
            c = counter.display()
        ),
    )
    .expect("write fake df");
    std::fs::set_permissions(
        &fake,
        <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o755),
    )
    .expect("chmod");

    let out = std::process::Command::new("bash")
        .arg(root.join("infra/gate.sh"))
        .arg("--quick")
        .env("BOSS_GATE_DF_CMD", fake.to_str().expect("utf8"))
        .env("BOSS_GATE_MIN_FREE_GB", "12")
        .env("BOSS_GATE_RECEIPT", receipt.to_str().expect("utf8"))
        .current_dir(&root)
        .output()
        .expect("run gate.sh");
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    let body = std::fs::read_to_string(&receipt).unwrap_or_else(|e| {
        panic!("a mid-run refusal must still write a receipt ({e}).\nstderr: {stderr}")
    });
    let _ = std::fs::remove_dir_all(&dir);

    let parsed: serde_json::Value =
        serde_json::from_str(&body).unwrap_or_else(|e| panic!("receipt is not JSON ({e}): {body}"));
    let checks = parsed
        .get("checks")
        .and_then(|c| c.as_array())
        .unwrap_or_else(|| panic!("receipt carries no checks array: {body}"))
        .clone();
    assert!(
        !checks.is_empty(),
        "`fmt` ran before the refusal, so the receipt must hold it: {body}"
    );
    for c in &checks {
        assert!(
            c.get("name").and_then(|n| n.as_str()).is_some(),
            "every check keeps its name: {body}"
        );
        assert!(
            c.get("result").and_then(|r| r.as_str()).is_some(),
            "every check keeps its result: {body}"
        );
        assert!(
            c.get("seconds")
                .and_then(serde_json::Value::as_u64)
                .is_some(),
            "every check must record how long it took — without it a starved \
             check and a broken one read the same: {body}"
        );
    }
}
