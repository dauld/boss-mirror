//! The rust gate has ONE definition: `infra/gate.sh`.
//!
//! On the 2026-08-10 train (PR #226) the gate's definition lived twice —
//! once in `.github/workflows/ci.yml`, once in whatever the agent ran
//! locally before pushing a car — and drifted twice in one day: a car
//! gated with named test files missed a lib-suite pin, and a car gated
//! with full crate suites missed a shell lint only CI ran. CLAUDE.md
//! §9a: collapse the pair, and pin what cannot collapse.
//!
//! The collapse: ci.yml's rust job invokes `infra/gate.sh` instead of
//! inlining cargo commands and lint scripts, so CI and a local run are
//! the same definition. What cannot collapse is pinned here:
//! - ci.yml must actually call the script, and must not grow a second
//!   inline definition beside it (a new `run: infra/lint/...` line in
//!   the rust job is the pair reopening);
//! - the script must keep covering the checks the gate exists to run —
//!   a trimmed roster is exactly the under-covering gate that let both
//!   #226 failures through.
//!
//! Both tests name the offending entry when they fail.

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

/// The rust-job slice of ci.yml: from the `rust:` job key to the next
/// top-level job key.
fn rust_job() -> String {
    let ci = read(".github/workflows/ci.yml");
    let start = ci.find("\n  rust:").expect("ci.yml has a rust job");
    let rest = &ci[start + 1..];
    let end = rest.find("\n  web:").unwrap_or(rest.len());
    rest[..end].to_string()
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

#[test]
fn ci_rust_job_invokes_the_gate_script() {
    let job = rust_job();
    assert!(
        job.contains("infra/gate.sh"),
        "ci.yml's rust job does not invoke infra/gate.sh — the gate's \
         definition has forked away from the script"
    );
}

#[test]
fn ci_rust_job_has_no_inline_second_definition() {
    let job = rust_job();
    // Environment setup stays in ci.yml (toolchain, cache, schema
    // apply); checks do not. An inline check line beside the script
    // call is the two-definition state this test exists to prevent.
    let inline_checks = [
        "run: cargo clippy",
        "run: cargo test",
        "run: cargo fmt",
        "run: infra/lint/",
    ];
    for needle in inline_checks {
        assert!(
            !job.contains(needle),
            "ci.yml's rust job inlines `{needle}` beside infra/gate.sh — \
             the gate now has two definitions again; move the check into \
             the script"
        );
    }
}

/// The forge workflow is the one that actually gates a train — since
/// the 2026-08-12 cutover, `.github/workflows/ci.yml` runs on the
/// public mirror while every car lands through Forgejo. It ran
/// locomotive + fmt + clippy + migrate + build + test and NOT the
/// script, so the whole lint roster was unenforced in production for a
/// day and thirteen trains landed green over a real `no-wallclock`
/// violation. The pin above only ever knew about the GitHub file,
/// which is why nothing caught it. It knows about both now.
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
    // Same rule as the GitHub job: environment setup (services, schema
    // apply) stays in the workflow, checks live in the script. The
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

    // The lint roster used to be hand-listed here, and by 2026-08-13 it
    // had drifted to a strict subset: dispatcher-rules-ratchet,
    // schema-converge and no-secrets all ran in gate.sh while this test
    // said nothing about them, so any of the three — including the
    // secret scanner — could have been deleted from the gate silently.
    // That is the same under-covering shape PR #226 shipped twice, just
    // one level up, so the roster is derived instead of restated
    // (CLAUDE.md §9a).
    //
    // Every executable check in infra/lint/ must appear in gate.sh
    // unless it is listed below with a reason. The directory does hold
    // legitimate non-gate scripts — that was the original objection to
    // globbing — but "which ones and why" is a decision that should be
    // written down once, here, rather than expressed as absence.
    let not_gated: &[(&str, &str)] = &[
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
    ];

    let mut missing = Vec::new();
    for entry in std::fs::read_dir(repo_root().join("infra/lint")).expect("read infra/lint") {
        let path = entry.expect("dir entry").path();
        let name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) if n.ends_with(".sh") => n.to_string(),
            _ => continue,
        };
        if not_gated.iter().any(|(n, _)| *n == name) {
            continue;
        }
        if !gate.contains(&name) {
            missing.push(name);
        }
    }
    missing.sort();
    assert!(
        missing.is_empty(),
        "infra/lint/ holds check(s) that infra/gate.sh does not run: {missing:?}. \
         Either add them to the gate, or add them to `not_gated` in this test \
         with the reason they are exempt."
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
    let dir = std::env::temp_dir().join("boss-gate-headroom-poll");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir");
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
    for needle in ["infra/postgres/schema/", "rules\\.toml", "/seeds/"] {
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

    assert!(
        hook.contains("gate.sh") && hook.contains("--quick"),
        "the hook must invoke the pre-flight, or it is a file that does nothing"
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
