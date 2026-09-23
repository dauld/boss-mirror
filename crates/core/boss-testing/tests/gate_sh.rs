//! The rust gate has ONE definition: `infra/gate.sh`.
//!
//! On the 2026-08-10 train (PR #226) the gate's definition lived twice —
//! once in `.github/workflows/ci.yml`, once in whatever the agent ran
//! locally before pushing a car — and drifted twice in one day: a car
//! gated with named test files missed a lib-suite pin, and a car gated
//! with full crate suites missed a shell lint only CI ran. CLAUDE.md
//! §9a: collapse the pair, and pin what cannot collapse.
//!
//! The collapse: the gate runner (`infra/gate-runner/run.sh`) invokes
//! `infra/gate.sh` for every car and — since 2026-09-13, design 128b5496
//! — for every train, so a gate and a local run are the same definition.
//! The CI workflow's `test` job used to be the train's copy of it; that
//! job is gone. What cannot collapse is pinned here:
//! - the runner must actually call the script, and the workflow must
//!   not grow a second inline definition (a `test` job or a
//!   `run: infra/lint/...` line is the pair reopening);
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

use boss_testing::repo_root;

fn read(rel: &str) -> String {
    let path = repo_root().join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// `bash infra/gate.sh <args>` in this tree, READABLE BY WHATEVER UID
/// RUNS THE TEST.
///
/// WHY, measured on 2026-09-11 (packet `b048c511`). Since that day the
/// gate runs as uid 65534 and every builder brief says to verify work
/// under `setpriv --reuid=65534`. The dev pod's checkout is root-owned
/// (`drwxrwsr-x root 1500`), so since git 2.35.2 every git command run
/// by any other uid refuses it:
///
///     fatal: detected dubious ownership in repository at '/work/boss'
///
/// `--auto` derives its scope from git, so the refusal became "found no
/// change at all against HEAD~1" and the gate declined before the poll
/// this file tests could happen: 12 of 13 as 65534, 13 of 13 as root.
/// A test ABOUT the gate that cannot pass the verification step the
/// gate itself demands leaves a builder choosing between ignoring a red
/// and stopping, which is the whole packet.
///
/// The fix is git's own env-var config channel, and it reaches every
/// git in the child tree rather than only gate.sh's own calls —
/// measured as 65534 in this tree: `--roster` and `--self-test` make 3
/// git calls each (trunk-candidate `rev-parse`, discarded in those
/// modes), `--quick` makes 223 of which 34 hit the refusal, and without
/// this env `--quick` exits 1 naming three pre-flight lints that read
/// history — `migrations-append-only`, `no-secrets`,
/// `steptype-bundle-ratchet` — while the receipt's `rev-parse HEAD`
/// degrades to `"head": "unknown"`. With the env supplied, the same
/// `--quick` as 65534 exits 0 and no ownership refusal is left.
///
/// SCOPED TO THE RESOLVED `repo_root()`, never `*`: blessing one known
/// path is a statement about this checkout, while a wildcard would bless
/// every repository a test ever reaches — including the throwaway clones
/// the sibling suites build.
///
/// Appended AFTER whatever `GIT_CONFIG_*` entries the parent already
/// exports, the way `boss-cli`'s `git_auth::apply_at` does, because
/// `infra/cluster/manifests/boss-dev.yaml` uses slot 0 for the forge
/// credential helper: a flat `GIT_CONFIG_COUNT=1` here would silently
/// take that helper away from the child.
///
/// WHY `safe.directory` IS RIGHT HERE AND WRONG IN
/// `infra/forge/delete-orphan-object.sh`, which rejected it by name and
/// drops to the tree's owner instead: that verb runs as root inside
/// somebody else's clone, and a root WRITE there leaves root-owned
/// objects that break the owner's later pulls — silencing the ownership
/// check would buy its read at the price of making that hazard reachable
/// by the next edit. Nothing in this direction writes: `--auto` diffs,
/// the receipt reads `HEAD`, the lints read history. That is the same
/// distinction `infra/forge/publish-github-pr.sh` drew when it DID
/// accept `safe.directory` for its fetch. Do not "tidy" this into a
/// wildcard, and do not copy it to a path that writes.
fn gate_cmd(args: &[&str]) -> std::process::Command {
    let root = repo_root();
    let slot = std::env::var("GIT_CONFIG_COUNT")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(0);
    let mut cmd = std::process::Command::new("bash");
    cmd.arg(root.join("infra/gate.sh"))
        .args(args)
        .env("GIT_CONFIG_COUNT", (slot + 1).to_string())
        .env(format!("GIT_CONFIG_KEY_{slot}"), "safe.directory")
        .env(format!("GIT_CONFIG_VALUE_{slot}"), &root)
        .current_dir(&root);
    cmd
}

/// Since 2026-09-13 (design 128b5496) the workflow has NO `test` job:
/// the Rust checks run as the train's cluster gate-run, which is
/// `infra/gate-runner/run.sh` invoking `infra/gate.sh` — the same one
/// definition every car's gate runs. What the workflow must not do is
/// grow the second definition back: no `test` job, no inline cargo or
/// lint invocation anywhere in it.
fn forge_workflow() -> String {
    read(".forgejo/workflows/ci.yml")
}

/// The gate runner — the thing that now judges a train's Rust — runs
/// the script, not a copy of its checks.
#[test]
fn the_gate_runner_invokes_the_gate_script() {
    let run = read("infra/gate-runner/run.sh");
    assert!(
        run.contains("./infra/gate.sh"),
        "infra/gate-runner/run.sh does not invoke infra/gate.sh — the runner that gates \
         every car and every train has forked away from the gate's definition"
    );
}

#[test]
fn the_forge_workflow_carries_no_second_definition_of_the_gate() {
    let ci = forge_workflow();
    assert!(
        !ci.contains("\n  test:") && !ci.contains("\n  fast:"),
        ".forgejo/workflows/ci.yml has a test or fast job again — the Rust checks run as \
         the train's cluster gate (128b5496); a second run of them here is the pair reopening"
    );
    let inline_checks = [
        "run: cargo clippy",
        "run: cargo test",
        "run: cargo build",
        "run: cargo fmt",
        "run: infra/lint/",
        "run: infra/gate.sh",
    ];
    for needle in inline_checks {
        assert!(
            !ci.contains(needle),
            ".forgejo/workflows/ci.yml inlines `{needle}` — the gate has two definitions \
             again; the checks live in infra/gate.sh, run by the gate runner"
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
    // slipping into that exclusion set. Until 2026-09-18 the set was
    // pinned by a second hand-typed copy of it HERE — one of FIVE copies
    // the tech-debt audit counted (H9, backlog 6fa15484): gate.sh's
    // array, this list, the conductor's compiled fallback, the
    // delivery-policy seed row and the live registry row, with nothing
    // holding gate.sh's copy equal to the conductor's. Now each excluded
    // lint declares its own exclusion in its header (`# consist: skip —
    // <why>`), gate.sh derives the set from those (`--exclusions`), and
    // the conductor asks the assembled tree's gate.sh for its roster.
    // So what is pinned is the DERIVATION: the roster is exactly the
    // directory minus what the lints themselves declare, asked of the
    // script rather than re-parsed from its text — a second parser
    // would be the pair reopening.
    let excluded = exclusions_of(&mut gate_cmd(&["--exclusions"]));
    assert!(
        !excluded.is_empty(),
        "no lint declares a consist skip — the four that need a live database, a \
         built workspace or a package manager must still say so in their headers"
    );

    let out = gate_cmd(&["--roster"])
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

    let mut disagree = Vec::new();
    for entry in std::fs::read_dir(repo_root().join("infra/lint")).expect("read infra/lint") {
        let path = entry.expect("dir entry").path();
        let name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) if n.ends_with(".sh") => n.to_string(),
            _ => continue,
        };
        let rel = format!("infra/lint/{name}");
        let declared_skip = excluded.iter().any(|(p, _)| *p == rel);
        let runs = preflighted.iter().any(|p| *p == rel);
        if declared_skip == runs {
            disagree.push(name);
        }
    }
    disagree.sort();
    assert!(
        disagree.is_empty(),
        "infra/lint/ and gate.sh's pre-flight disagree on: {disagree:?}. A lint is out \
         of the pre-flight exactly when its own header declares `# consist: skip — <why>`, \
         and `--exclusions` must print exactly those."
    );
    assert!(
        gate.contains("infra/lint/svelte-check.sh"),
        "svelte-check.sh is kept out of the pre-flight for cost, not for coverage: \
         the gate's web phase must still run it"
    );
    // Not `contains("infra/lint/no-snapshot-arrays.sh")` — the exclusion
    // list already names the path, so that would pass with the lint
    // never run. From 2026-08-31 to 2026-09-12 the lint was excluded
    // here "because CI builds, then runs it", and nothing ran it: it
    // named a page deleted in #161 and stayed red, unread, for twelve
    // days (docs/invariants/spa-lists-are-generated.toml recorded the
    // gap as `unenforced`). A check nobody runs is not running.
    assert!(
        gate.contains("check \"no-snapshot-arrays\""),
        "no-snapshot-arrays.sh is kept out of the pre-flight because it needs the built \
         boss-ports-list: the gate's build phase must still run it as a check"
    );
}

/// `gate.sh --exclusions`, parsed: one `(path, why)` per line, the two
/// separated by a tab because a reason has spaces in it.
fn exclusions_of(cmd: &mut std::process::Command) -> Vec<(String, String)> {
    let out = cmd.output().expect("run gate.sh --exclusions");
    assert!(
        out.status.success(),
        "gate.sh --exclusions refused: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| {
            let (path, why) = l
                .split_once('\t')
                .unwrap_or_else(|| panic!("an exclusion line is `<path>\\t<why>`, got {l:?}"));
            (path.to_string(), why.to_string())
        })
        .collect()
}

/// A bare tree holding THIS tree's gate.sh and only the lints a test
/// puts there, so the derivation can be exercised on lints written for
/// the purpose rather than on whatever `infra/lint/` holds today. The
/// gate's first-pinned lint is stubbed because the roster refuses a
/// tree without it, which is a different claim.
fn skeleton(label: &str, lints: &[(&str, &str)]) -> std::path::PathBuf {
    let dir = boss_testing::scratch_dir(label);
    let tree = dir.join("tree");
    boss_testing::create_dir(&tree.join("infra/lint/lib"));
    // gate.sh refuses to run without any helper it sources — the lint
    // vocabulary (LINT_CANNOT_ANSWER) and the target-dir rule (backlog
    // 955c99b6) — so the skeleton carries the gate and all of them.
    boss_testing::copy_gate_sh(&tree);
    boss_testing::write_file(
        &tree.join("infra/lint/workspace-declares-what-it-runs.sh"),
        "#!/usr/bin/env bash\nexit 0\n",
    );
    for (name, body) in lints {
        boss_testing::write_file(&tree.join("infra/lint").join(name), body);
    }
    tree
}

fn skeleton_gate(tree: &std::path::Path, mode: &str) -> std::process::Command {
    let mut cmd = std::process::Command::new("bash");
    cmd.arg(tree.join("infra/gate.sh"))
        .arg(mode)
        .current_dir(tree);
    cmd
}

/// THE ONE DEFINITION OF "NOT PRE-FLIGHTED" IS THE LINT'S OWN HEADER.
///
/// A lint that needs something a bare tree cannot answer in seconds — a
/// live database, a built workspace, a package manager — says so on a
/// header line, and that line is the whole mechanism: gate.sh reads it
/// to build the roster, prints it on `--exclusions`, and the conductor
/// asks gate.sh. Nothing else in the tree lists the excluded lints, so
/// nothing else can drift from this.
#[test]
fn a_lint_declares_its_own_consist_skip_in_its_header() {
    let tree = skeleton(
        "boss-gate-consist-skip",
        &[
            (
                "declared.sh",
                "#!/usr/bin/env bash\n\
                 # A lint that sweeps the live database.\n\
                 #\n\
                 # consist: skip — psql against a live database, not a question about a tree\n\
                 exit 0\n",
            ),
            (
                "plain.sh",
                "#!/usr/bin/env bash\n# An ordinary static check.\nexit 0\n",
            ),
            (
                "late.sh",
                "#!/usr/bin/env bash\n\
                 set -euo pipefail\n\
                 # consist: skip — below the first line of code, so prose, not a declaration\n\
                 exit 0\n",
            ),
        ],
    );

    let excluded = exclusions_of(&mut skeleton_gate(&tree, "--exclusions"));
    assert_eq!(
        excluded,
        vec![(
            "infra/lint/declared.sh".to_string(),
            "psql against a live database, not a question about a tree".to_string()
        )],
        "exactly the lint whose HEADER declares the skip, with its reason; a marker \
         below the first line of code is prose"
    );

    let out = skeleton_gate(&tree, "--roster")
        .output()
        .expect("run the skeleton's gate.sh --roster");
    assert!(
        out.status.success(),
        "--roster refused: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let roster = String::from_utf8_lossy(&out.stdout);
    let paths: Vec<&str> = roster
        .lines()
        .filter_map(|l| l.split_once(' ').map(|(_, p)| p))
        .collect();
    assert_eq!(
        paths,
        vec![
            "infra/lint/workspace-declares-what-it-runs.sh",
            "infra/lint/late.sh",
            "infra/lint/plain.sh",
        ],
        "the roster is the directory minus what the lints themselves declare"
    );
    let _ = std::fs::remove_dir_all(tree.parent().expect("skeleton has a parent"));
}

/// An exemption nobody explained is one nobody can later judge — the
/// rule the delivery-policy row used to enforce on its JSON, kept at
/// the one place the declaration now lives. Refused loudly, by name,
/// in both modes that derive from it: a bare `# consist: skip` must
/// not quietly drop a lint out of every gate.
#[test]
fn a_consist_skip_with_no_reason_is_refused() {
    let tree = skeleton(
        "boss-gate-consist-skip-mute",
        &[("mute.sh", "#!/usr/bin/env bash\n# consist: skip\nexit 0\n")],
    );
    for mode in ["--exclusions", "--roster"] {
        let out = skeleton_gate(&tree, mode)
            .output()
            .unwrap_or_else(|e| panic!("run the skeleton's gate.sh {mode}: {e}"));
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            !out.status.success(),
            "{mode} accepted a consist skip with no reason: {}",
            String::from_utf8_lossy(&out.stdout)
        );
        assert!(
            stderr.contains("infra/lint/mute.sh") && stderr.contains("consist: skip"),
            "{mode}'s refusal names the lint and the line it wants: {stderr}"
        );
    }
    let _ = std::fs::remove_dir_all(tree.parent().expect("skeleton has a parent"));
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
    // THE ONE GATE INVOCATION IN THIS FILE THAT DOES NOT GO THROUGH
    // `gate_cmd`, and deliberately: a floor no disk can satisfy refuses
    // at `require_headroom "to start"`, which runs BEFORE the trunk
    // derivation — measured as 65534 in a root-owned tree, this mode
    // makes exactly 0 git calls, so it has no ownership dependency to
    // supply for. That ordering is not incidental; it is what
    // `headroom_is_checked_before_the_scope_is_derived` pins. Stating
    // the boundary here beats spraying the env over an invocation that
    // never reaches git, which would hide the dependency instead.
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
    let dir = boss_testing::scratch_dir("boss-gate-headroom-poll");
    let counter = dir.join("calls");
    let fake = dir.join("df");
    // 1st call: 900GB free. Every later call: 1GB.
    boss_testing::write_exec(
        &fake,
        &format!(
            "#!/usr/bin/env bash\n\
             n=$(cat {c} 2>/dev/null || echo 0)\n\
             echo $((n+1)) > {c}\n\
             echo 'Filesystem 1024-blocks Used Available Capacity Mounted on'\n\
             if [ \"$n\" -eq 0 ]; then echo '/dev/fake 1 1 943718400 1% /'; \
             else echo '/dev/fake 1 1 1048576 99% /'; fi\n",
            c = counter.display()
        ),
    );

    let out = gate_cmd(&["--auto"])
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

/// THE LISTINGS ANSWER BEFORE THE FLOOR. `--exclusions` and `--roster`
/// read no tree and run no check — they list `infra/lint/` and what its
/// headers declare — and the conductor's consist check asks the
/// assembled tree's gate.sh for them. Until 2026-09-18 the disk floor
/// ran before ANY mode dispatched, so at 9GB free on the conductor's
/// volume `gate.sh --exclusions` was refused with "9GB free, need
/// 12GB. Refusing to start." and the consist check recorded a failure
/// it could not judge (backlog 13700f6f; CLAUDE.md Diagnosis: an
/// infrastructure refusal is not a consist failure). The floor guards a
/// gate run, not a question about the roster — and the impossible
/// floor is set the way `the_gate_refuses_to_run_without_headroom`
/// sets it, so the two tests disagree only about the mode.
#[test]
fn the_listings_answer_below_the_disk_floor() {
    for mode in ["--exclusions", "--roster"] {
        let out = gate_cmd(&[mode])
            .env("BOSS_GATE_MIN_FREE_GB", "99999999")
            .output()
            .expect("run gate.sh");
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            out.status.success(),
            "`gate.sh {mode}` is a read-only listing and must answer below the disk floor \
             (the conductor's consist check reads it, and a refusal there was recorded as \
             a consist failure at 9GB free on 2026-09-18).\nstderr: {stderr}"
        );
        assert!(
            !stderr.contains("Refusing to start"),
            "the floor must not speak on a listing.\nstderr: {stderr}"
        );
        assert!(
            stdout.contains("infra/lint/"),
            "`gate.sh {mode}` answered nothing.\nstdout: {stdout}"
        );
    }
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
    let out = gate_cmd(&["--self-test"])
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
    let dir = boss_testing::scratch_dir("boss-gate-check-timing");
    let counter = dir.join("calls");
    let fake = dir.join("df");
    let receipt = dir.join("receipt.json");
    // Calls 1 (startup) and 2 (before `fmt`) see plenty; call 3 (before
    // the first lint) trips, which is what makes the gate write a
    // receipt holding exactly one, real, timed check.
    boss_testing::write_exec(
        &fake,
        &format!(
            "#!/usr/bin/env bash\n\
             n=$(cat {c} 2>/dev/null || echo 0)\n\
             echo $((n+1)) > {c}\n\
             echo 'Filesystem 1024-blocks Used Available Capacity Mounted on'\n\
             if [ \"$n\" -lt 2 ]; then echo '/dev/fake 1 1 943718400 1% /'; \
             else echo '/dev/fake 1 1 1048576 99% /'; fi\n",
            c = counter.display()
        ),
    );

    let out = gate_cmd(&["--quick"])
        .env("BOSS_GATE_DF_CMD", fake.to_str().expect("utf8"))
        .env("BOSS_GATE_MIN_FREE_GB", "12")
        .env("BOSS_GATE_RECEIPT", receipt.to_str().expect("utf8"))
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

/// `--auto` in a SCRATCH TREE: a shared clone of this checkout carrying
/// the working tree's `infra/gate.sh` (committed, so the clone is clean),
/// plus the files `touch` names as untracked scratch, gated with a `df`
/// that reports plenty at startup and trips at the first phase boundary
/// after it (`fixture`). The gate has derived its scope by then and the
/// refusal writes the receipt, so this reads what `--auto` DECIDED
/// without compiling anything — the same trick
/// `the_receipt_times_every_check` uses to get a receipt cheaply.
///
/// The gate under test is the one in THIS tree, not the one at HEAD: a
/// clone alone would gate the committed script and pass or fail about
/// the wrong version (a read without its version is a guess).
fn auto_scope_of(label: &str, touch: &[&str]) -> (String, serde_json::Value) {
    let root = repo_root();
    let dir = boss_testing::scratch_dir(label);
    let tree = dir.join("tree");
    let git = |args: &[&str], cwd: &std::path::Path| {
        let slot = std::env::var("GIT_CONFIG_COUNT")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(0);
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(cwd)
            .env("GIT_CONFIG_COUNT", (slot + 1).to_string())
            .env(format!("GIT_CONFIG_KEY_{slot}"), "safe.directory")
            .env(format!("GIT_CONFIG_VALUE_{slot}"), &root)
            .output()
            .unwrap_or_else(|e| panic!("git {}: {e}", args.join(" ")));
        assert!(
            out.status.success(),
            "git {} in {} failed: {}",
            args.join(" "),
            cwd.display(),
            String::from_utf8_lossy(&out.stderr)
        );
    };
    git(
        &[
            "clone",
            "--shared",
            "--quiet",
            root.to_str().expect("utf8"),
            tree.to_str().expect("utf8"),
        ],
        &dir,
    );
    std::fs::copy(root.join("infra/gate.sh"), tree.join("infra/gate.sh"))
        .expect("carry this tree's gate.sh into the scratch clone");
    git(
        &[
            "-c",
            "user.email=gate-scope@test",
            "-c",
            "user.name=gate-scope",
            "commit",
            "--quiet",
            "--allow-empty",
            "-am",
            "the gate under test",
        ],
        &tree,
    );
    for rel in touch {
        let path = tree.join(rel);
        boss_testing::create_dir(path.parent().expect("a scratch path has a parent"));
        boss_testing::write_file(&path, "-- scratch\n");
    }

    let counter = dir.join("calls");
    let fake = dir.join("df");
    boss_testing::write_exec(
        &fake,
        &format!(
            "#!/usr/bin/env bash\n\
             n=$(cat {c} 2>/dev/null || echo 0)\n\
             echo $((n+1)) > {c}\n\
             echo 'Filesystem 1024-blocks Used Available Capacity Mounted on'\n\
             if [ \"$n\" -lt 1 ]; then echo '/dev/fake 1 1 943718400 1% /'; \
             else echo '/dev/fake 1 1 1048576 99% /'; fi\n",
            c = counter.display()
        ),
    );
    let receipt = dir.join("receipt.json");
    let out = std::process::Command::new("bash")
        .arg(tree.join("infra/gate.sh"))
        .arg("--auto")
        .current_dir(&tree)
        .env("BOSS_GATE_DF_CMD", fake.to_str().expect("utf8"))
        .env("BOSS_GATE_MIN_FREE_GB", "12")
        .env("BOSS_GATE_RECEIPT", receipt.to_str().expect("utf8"))
        .env("BOSS_GATE_TRUNK", "HEAD")
        .output()
        .expect("run gate.sh --auto in the scratch clone");
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    let body = std::fs::read_to_string(&receipt).unwrap_or_else(|e| {
        panic!(
            "the refusal at the first phase must still write a receipt ({e}).\nstdout: {stdout}\nstderr: {stderr}"
        )
    });
    let _ = std::fs::remove_dir_all(&dir);
    let parsed: serde_json::Value = serde_json::from_str(&body)
        .unwrap_or_else(|e| panic!("receipt is not JSON ({e}): {body}\nstderr: {stderr}"));
    (stdout, parsed)
}

fn scope_of(receipt: &serde_json::Value) -> Vec<String> {
    receipt
        .get("scope")
        .and_then(|s| s.as_str())
        .unwrap_or_else(|| panic!("receipt carries no scope: {receipt}"))
        .split_whitespace()
        .map(str::to_string)
        .collect()
}

/// Does this crate stand up the shared schema in a test? `TestDb::new`
/// (and `new_without`) is the one constructor that applies
/// `infra/postgres/schema/` to a fresh database, so a crate that calls
/// it reads every migration, whatever its test files are named and
/// whether or not its manifest declares a `postgres` feature.
fn stands_up_the_schema(crate_name: &str) -> bool {
    let root = repo_root();
    let manifest = std::fs::read_dir(root.join("crates"))
        .expect("crates/")
        .filter_map(Result::ok)
        .map(|tier| tier.path().join(crate_name))
        .find(|p| p.join("Cargo.toml").is_file())
        .unwrap_or_else(|| panic!("{crate_name} is not a crate under crates/*/"));
    fn mentions(dir: &std::path::Path) -> bool {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return false;
        };
        entries.filter_map(Result::ok).any(|e| {
            let p = e.path();
            if p.is_dir() {
                mentions(&p)
            } else {
                p.extension().is_some_and(|x| x == "rs")
                    && std::fs::read_to_string(&p)
                        .map(|s| s.contains("TestDb::new"))
                        .unwrap_or(false)
            }
        })
    }
    mentions(&manifest.join("src")) || mentions(&manifest.join("tests"))
}

const SCRATCH_MIGRATION: &str =
    "infra/postgres/schema/99991231235959-a-scratch-migration-gates-its-readers.sql";

/// THE PACKET (backlog 4711828d). On 2026-09-16 two cars each added a
/// credentials-registry migration; `--auto` scoped each to the crates
/// whose FILES changed (boss-dispatcher-handlers, boss-testing) and never
/// ran boss-jobs, whose `credentials_pg.rs` pins the seeded rows. Both
/// gated green (receipts 4c2f15c5, 719b6e61); the train gate ran boss-jobs
/// on the assembled tree and struck all six cars aboard (train 767cfb14,
/// gate-run 06995f6c). A schema change is a change to every crate that
/// stands up the schema, and the scope has to say so.
#[test]
fn a_migration_only_car_scopes_every_crate_that_stands_up_the_schema() {
    let (stdout, receipt) = auto_scope_of("gate-scope-migration-only", &[SCRATCH_MIGRATION]);
    let scope = scope_of(&receipt);
    // The crate from the incident, and the crate that owns the fixture.
    for must in ["boss-jobs", "boss-testing"] {
        assert!(
            scope.iter().any(|c| c == must),
            "a migration-only car must scope {must} — the car that struck train 767cfb14 \
             gated without it.\nscope: {scope:?}\nstdout: {stdout}"
        );
    }
    // The HONEST predicate, not the convenient one: boss-dispatcher
    // declares no `postgres` feature and still stands up a TestDb in
    // tests/rules_wait_pg.rs. A derivation keyed on the feature flag
    // would drop it and read 13 crates where 25 read the schema.
    assert!(
        scope.iter().any(|c| c == "boss-dispatcher"),
        "boss-dispatcher stands up the schema without a `postgres` feature; a scope \
         that omits it was derived from the manifest instead of from the tests.\n\
         scope: {scope:?}"
    );
    // Every crate pulled in is one that reads the schema — nothing rides
    // in on a name or a list.
    for c in &scope {
        assert!(
            stands_up_the_schema(c),
            "{c} was scoped by a schema change but constructs no TestDb — the \
             derivation named a crate that does not read the schema.\nscope: {scope:?}"
        );
    }
    assert!(
        scope.len() >= 10,
        "the schema readers came back suspiciously few ({}) — a grep that matches \
         nothing is a map that covers nothing.\nscope: {scope:?}",
        scope.len()
    );
    // The receipt names WHY: the migration it saw and the crates it
    // pulled in for it, so a reader of the packet can tell "scoped
    // because the tree changed these crates" from "scoped because the
    // schema moved".
    let why = receipt
        .get("schema_change")
        .unwrap_or_else(|| panic!("receipt carries no schema_change field: {receipt}"));
    let paths: Vec<&str> = why
        .get("paths")
        .and_then(|p| p.as_array())
        .unwrap_or_else(|| panic!("schema_change carries no paths array: {receipt}"))
        .iter()
        .filter_map(|p| p.as_str())
        .collect();
    assert_eq!(
        paths,
        vec![SCRATCH_MIGRATION],
        "schema_change.paths must name the migration the gate saw: {receipt}"
    );
    let readers: Vec<String> = why
        .get("readers")
        .and_then(|r| r.as_str())
        .unwrap_or_else(|| panic!("schema_change carries no readers: {receipt}"))
        .split_whitespace()
        .map(str::to_string)
        .collect();
    assert_eq!(
        readers, scope,
        "for a migration-only car the scope IS the readers, and the receipt must say \
         so in one place: {receipt}"
    );
    assert!(
        stdout.contains("schema change ->"),
        "the gate must say on stdout that the schema change is what widened the \
         scope.\nstdout: {stdout}"
    );
}

/// A migration beside a crate change scopes BOTH: the crate the tree
/// changed and the crates that read the schema. And the contrast — a
/// crate change with no migration pulls no reader in — pins that the
/// widening is keyed on `infra/postgres/schema/`, not on every car.
#[test]
fn a_migration_beside_a_crate_change_scopes_both() {
    // boss-expr constructs no TestDb, so it can only enter the scope
    // through its own changed file.
    assert!(
        !stands_up_the_schema("boss-expr"),
        "this test needs a crate that does NOT read the schema; pick another"
    );
    let (stdout, receipt) = auto_scope_of(
        "gate-scope-migration-and-crate",
        &[SCRATCH_MIGRATION, "crates/core/boss-expr/src/zz_scratch.rs"],
    );
    let scope = scope_of(&receipt);
    for must in ["boss-expr", "boss-jobs"] {
        assert!(
            scope.iter().any(|c| c == must),
            "a migration beside a boss-expr change must scope {must}.\nscope: {scope:?}\n\
             stdout: {stdout}"
        );
    }

    let (stdout, receipt) = auto_scope_of(
        "gate-scope-crate-only",
        &["crates/core/boss-expr/src/zz_scratch.rs"],
    );
    let scope = scope_of(&receipt);
    assert_eq!(
        scope,
        vec!["boss-expr".to_string()],
        "a crate change with no migration must not pull the schema readers in — \
         that would make every car a whole-workspace gate.\nstdout: {stdout}"
    );
    let paths = receipt
        .get("schema_change")
        .and_then(|w| w.get("paths"))
        .and_then(|p| p.as_array())
        .cloned()
        .unwrap_or_default();
    assert!(
        paths.is_empty(),
        "no migration changed, so schema_change.paths must be empty: {receipt}"
    );
}

/// THE PACKET (backlog f532c345). The platform bundle grew two more
/// registries on 2026-09-18 — `infra/platform/stations/` (H4 car 1) and
/// `infra/platform/step-plugins/` (car 3), each held equal to the
/// migrations by a `*_bundle_is_the_migrations_pg.rs` pin in boss-jobs —
/// and the gate's `path_shapes` still named only `workflows/`. Measured
/// on main at #452 through the gate's own `path_map`:
/// `infra/platform/stations/repair.toml` and
/// `infra/platform/step-plugins/sign-off.toml` each derived NO crate,
/// while `infra/platform/workflows/gate-run.toml` derived boss-jobs. The
/// three bundle cars gated `boss-cli boss-jobs boss-testing` (receipts on
/// gate-runs dd939905, 531ab449, 0c6e6ac9) only because each also
/// changed Rust; a bundle-ONLY car — the ordinary kind, once the bundle
/// is the registry — would have gated lints-only and never run the pin
/// that exists to reject it (the class "a green gate only covers what it
/// runs"). Same hole, same fix as the tenant bundle (b59efe54): the
/// shape is derived from the DIRECTORY, so this walks `infra/platform/`
/// and gates one scratch row in each bundle it finds — a fourth bundle
/// is covered the day its directory appears, and a bundle the map
/// forgets names itself here.
#[test]
fn a_platform_bundle_edit_scopes_the_crate_whose_pins_hold_it_to_the_migrations() {
    let root = repo_root();
    let mut bundles: Vec<String> = std::fs::read_dir(root.join("infra/platform"))
        .expect("infra/platform/ is the platform bundle")
        .filter_map(Result::ok)
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    bundles.sort();
    assert!(
        bundles.len() >= 3,
        "infra/platform/ holds fewer bundle directories ({bundles:?}) than the three that \
         existed when this test was written — a walk that finds nothing pins nothing"
    );
    for bundle in &bundles {
        let scratch = format!("infra/platform/{bundle}/zz-a-scratch-row.toml");
        let (stdout, receipt) = auto_scope_of(&format!("gate-scope-bundle-{bundle}"), &[&scratch]);
        let scope = scope_of(&receipt);
        assert!(
            scope.iter().any(|c| c == "boss-jobs"),
            "a car that only edits {scratch} must scope boss-jobs — that crate holds \
             the bundle equal to the migrations, and a bundle-only car that gates \
             lints-only never runs the one test that can reject it.\n\
             scope: {scope:?}\nstdout: {stdout}"
        );
        assert!(
            stdout.contains("--auto scoping to"),
            "the gate must say it scoped for {scratch}, not fall to lints-only.\n\
             stdout: {stdout}"
        );
    }
}

/// THE PACKET (backlog 1b52c278). Train #470's car 4f1ba1f9 edited
/// `docs/tenant-contract.md` (one sentence on the tenant.toml row)
/// without touching `CONTRACT` in boss-cli's tenant.rs. The equality pin
/// `the_contract_doc_carries_the_codes_table` lives in boss-cli, which
/// the car did not change, so neither the car's gate nor the train gate
/// (both `--auto`, both scoped to boss-gateway) ran it; origin/main went
/// red on that pin at 04:38Z and struck the next car gated on top of it
/// (5e4951a4) — the FIRST time the pin ran at all.
///
/// Measured through the gate's own `file_input_index` at #471: 191
/// (path, crate) pairs, and `docs/tenant-contract.md` in none of them.
/// The index counted a repo-relative literal only inside a crate's
/// `tests/` directory and, elsewhere, only a `../`-escaping one — and
/// the pin is a `#[cfg(test)]` module under `src/` that reads the doc
/// through `boss_testing::repo_root().join("docs/tenant-contract.md")`,
/// which is neither. Eight (path, crate) pairs sat outside the map for
/// the same reason (estate.toml, sor-ports.env, pod-build.env,
/// as-gate-uid.sh, access.toml, tax.toml, this doc). The idiom is the
/// discriminator the index lacked: a literal that is the argument of
/// `.join(` is a path being read, not a sentence that mentions one, so
/// the gate now counts it under `src/` too. This gates a scratch edit to
/// the doc and asserts the crate holding its pin is in scope.
#[test]
fn a_doc_a_test_module_reads_by_repo_path_scopes_the_crate_that_pins_it() {
    // The doc's path is read off the pin's own source rather than
    // restated here: a repo-path literal in this file would be a second
    // reader the index counts (this crate), and a pin that stops
    // reading the doc must fail by name rather than pass about nothing.
    let pin = read("crates/orchestrators/boss-cli/src/tenant.rs");
    let doc = pin
        .split("repo_root().join(\"")
        .skip(1)
        .filter_map(|rest| rest.split('"').next())
        .find(|p| p.starts_with("docs/"))
        .unwrap_or_else(|| {
            panic!(
                "boss-cli's tenant.rs no longer reads a doc through repo_root().join(\"docs/…\") — \
                 move this test to whichever crate pins the contract doc now"
            )
        })
        .to_string();
    let (stdout, receipt) = auto_scope_of("gate-scope-doc-read-by-a-pin", &[&doc]);
    let scope = scope_of(&receipt);
    assert!(
        scope.iter().any(|c| c == "boss-cli"),
        "a car that only edits {doc} must scope boss-cli — that crate's tenant.rs holds \
         the doc's table equal to CONTRACT, and a docs-only car that gates lints-only \
         never runs the pin that exists to reject it (train #470).\n\
         scope: {scope:?}\nstdout: {stdout}"
    );
    assert!(
        stdout.contains("--auto scoping to"),
        "the gate must say it scoped for {doc}, not fall to lints-only.\nstdout: {stdout}"
    );
}

// ---- the hosting door's gate half (a479faf7; design 01c3cc3f reader 3) ----

/// The lint the gate runs the edit level through, by name.
const EDIT_LEVEL_LINT: &str = "a-car-stays-under-the-edit-level";

/// A synthetic tree carrying this tree's gate, its lint libs, the REAL
/// tier map and the edit-level lint; `main` holds the tree, a `car`
/// branch adds one core file and one doc. The instance is a one-shot
/// HTTP server on a loopback port answering `edit_level` as the case
/// says — the lint reads it the way the gate's other live lint reads
/// the registry, through `BOSS_JOBS_URL`.
struct LevelTree {
    dir: std::path::PathBuf,
    tree: std::path::PathBuf,
}

impl LevelTree {
    fn new(tag: &str) -> LevelTree {
        let dir = boss_testing::scratch_dir(&format!("gate-edit-level-{tag}"));
        let _ = std::fs::remove_dir_all(&dir);
        let tree = dir.join("tree");
        boss_testing::copy_lint_libs(&tree);
        boss_testing::copy_gate_sh(&tree);
        for rel in [
            "infra/platform/tiers.toml",
            &format!("infra/lint/{EDIT_LEVEL_LINT}.sh"),
        ] {
            let to = tree.join(rel);
            boss_testing::create_dir(to.parent().unwrap());
            std::fs::copy(repo_root().join(rel), &to)
                .unwrap_or_else(|e| panic!("carry {rel} into the synthetic tree: {e}"));
        }
        boss_testing::write_file(
            &tree.join("infra/lint/workspace-declares-what-it-runs.sh"),
            "#!/usr/bin/env bash\nexit 0\n",
        );
        for web in ["apps/web", "libs/web-kit"] {
            boss_testing::create_dir(&tree.join(web));
        }
        let t = LevelTree { dir, tree };
        t.vcs(&["init", "-q", "-b", "main"]);
        t.vcs(&["add", "."]);
        t.commit("the tree");
        t.vcs(&["checkout", "-q", "-b", "car"]);
        for (rel, body) in [
            ("docs/a.md", "# a\n"),
            ("crates/core/boss-a/src/lib.rs", "// a\n"),
        ] {
            let to = t.tree.join(rel);
            boss_testing::create_dir(to.parent().unwrap());
            boss_testing::write_file(&to, body);
        }
        t.vcs(&["add", "."]);
        t.commit("the car");
        let bin = t.dir.join("bin");
        boss_testing::create_dir(&bin);
        for tool in ["cargo", "bun"] {
            boss_testing::write_exec(&bin.join(tool), "#!/usr/bin/env bash\nexit 0\n");
        }
        boss_testing::write_exec(
            &t.dir.join("df"),
            "#!/usr/bin/env bash\n\
             echo 'Filesystem 1024-blocks Used Available Capacity Mounted on'\n\
             echo '/dev/fake 1 1 943718400 1% /'\n",
        );
        t
    }

    fn vcs(&self, args: &[&str]) {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(&self.tree)
            .output()
            .unwrap_or_else(|e| panic!("git {args:?}: {e}"));
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn commit(&self, msg: &str) {
        self.vcs(&[
            "-c",
            "user.email=edit-level@test",
            "-c",
            "user.name=edit-level",
            "commit",
            "-q",
            "-m",
            msg,
        ]);
    }

    /// Serve `body` (a JSON object, or a 404 when `None`) to every
    /// request on a loopback port, in a thread that answers a bounded
    /// number of connections and then stops.
    fn instance(body: Option<&'static str>) -> String {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for stream in listener.incoming().take(8) {
                let Ok(mut s) = stream else { break };
                let mut buf = [0u8; 4096];
                let _ = s.read(&mut buf);
                let resp = match body {
                    Some(b) => format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n\
                         content-length: {}\r\nconnection: close\r\n\r\n{b}",
                        b.len()
                    ),
                    None => "HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\n\
                             connection: close\r\n\r\n"
                        .to_string(),
                };
                let _ = s.write_all(resp.as_bytes());
            }
        });
        format!("http://{addr}")
    }

    fn quick(&self, jobs_url: &str) -> std::process::Output {
        let path = std::env::var("PATH").unwrap_or_default();
        std::process::Command::new("bash")
            .arg(self.tree.join("infra/gate.sh"))
            .arg("--quick")
            .current_dir(&self.tree)
            .env("PATH", format!("{}:{path}", self.dir.join("bin").display()))
            .env("BOSS_GATE_DF_CMD", self.dir.join("df"))
            .env("BOSS_GATE_MIN_FREE_GB", "12")
            .env("BOSS_GATE_TRUNK", "main")
            .env("BOSS_TRUNK_REF", "main")
            .env("GIT_CEILING_DIRECTORIES", "")
            .env("BOSS_JOBS_URL", jobs_url)
            .output()
            .expect("run gate.sh --quick")
    }
}

impl Drop for LevelTree {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn both(out: &std::process::Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// THE REFUSAL. A car whose diff against the trunk touches a path
/// closer to the core than the instance's edit level admits is a red
/// naming the lint, the FIRST offending path, its tier and the level
/// with both ranks — the same sentence `boss dispatch` prints, from
/// the one predicate. A doc beside it is not named: only the first
/// path above.
#[test]
fn the_gate_refuses_a_car_whose_diff_crosses_the_edit_level_naming_the_path_and_the_level() {
    let tree = LevelTree::new("refuses");
    let out = tree.quick(&LevelTree::instance(Some(
        r#"{"edit_level":"tenants","manifest":"/opt/boss/tenant/seeds/tenant.toml"}"#,
    )));
    let t = both(&out);
    assert_eq!(
        out.status.code(),
        Some(1),
        "a crossing is the car's red:\n{t}"
    );
    assert!(t.contains(EDIT_LEVEL_LINT), "{t}");
    assert!(
        t.contains("edit level `tenants` (rank 3) does not admit `crates/core/boss-a/src/lib.rs`"),
        "names the level and the first offending path:\n{t}"
    );
    assert!(t.contains("`core` (rank 1)"), "names the path's tier:\n{t}");
    assert!(
        !t.contains("does not admit `docs/a.md`"),
        "only the first path above:\n{t}"
    );
    assert!(t.contains("check(s) failed"), "{t}");
}

/// THE CONTROLS. The same car under `core` is clean; an instance that
/// declares no level (`null`) or has no level door at all (404 — a
/// server from before this car) enforces nothing and says so; and a
/// dark instance is CANNOT ANSWER, which `--quick` warns about and the
/// gate proper refuses (the generic pin on live-reading lints).
#[test]
fn the_edit_level_lint_admits_under_core_under_no_level_and_under_no_level_door() {
    let tree = LevelTree::new("admits");
    for (tag, body) in [
        ("core", Some(r#"{"edit_level":"core","manifest":"/m"}"#)),
        ("null", Some(r#"{"edit_level":null,"manifest":null}"#)),
        ("404", None),
    ] {
        let out = tree.quick(&LevelTree::instance(body));
        let t = both(&out);
        assert_eq!(out.status.code(), Some(0), "[{tag}] admitted:\n{t}");
        assert!(t.contains("pre-flight: clean"), "[{tag}] {t}");
        if tag != "core" {
            assert!(
                t.contains("no edit level"),
                "[{tag}] says nothing is enforced rather than staying silent:\n{t}"
            );
        }
    }
    let out = tree.quick("http://[::1]:9");
    let t = both(&out);
    assert_eq!(out.status.code(), Some(0), "--quick warns:\n{t}");
    assert!(
        t.contains(&format!("WARNING — '{EDIT_LEVEL_LINT}' could not answer")),
        "{t}"
    );
}

/// A MODE THE PARSER ACCEPTS MUST BE NAMED WHERE MODES ARE LISTED.
///
/// THE COST, paid twice (packet 410e21e2). `--lint` is `--quick` plus a
/// clippy scoped to the crates the tree changed. Its own comment in
/// gate.sh records why it was added on 2026-08-28: a car went red on
/// clippy alone, costing a gate and a re-gate — about 22 minutes of
/// cluster time — for two unused imports. On 2026-09-21 an agent spent
/// a gate on a redundant closure, having run `--quick`, because the
/// flag that would have caught it appeared in exactly one place a
/// reader ever sees: the unknown-arg error, which you reach only by
/// getting it wrong.
///
/// THE PAIR (CLAUDE.md §9a). The set of modes lives in the `case` block
/// that parses them and in the usage header that advertises them. They
/// cannot be collapsed — one is control flow, the other is the comment
/// a reader opens the file for — so the pair is pinned, and this test
/// names the flag that drifted.
#[test]
fn every_mode_the_parser_accepts_is_named_in_the_usage_header() {
    let gate = read("infra/gate.sh");
    let lines: Vec<&str> = gate.lines().collect();

    let parse_at = lines
        .iter()
        .position(|l| l.contains("while [ $# -gt 0 ]; do"))
        .expect("infra/gate.sh no longer parses its arguments in a while loop");

    // The header is everything above the parser; the flags are the arms
    // of the `case`, read as `--name)`.
    let (header, parser) = lines.split_at(parse_at);
    let header = header.join("\n");

    let mut missing: Vec<&str> = Vec::new();
    let mut found = 0usize;
    for line in parser {
        let t = line.trim_start();
        let Some(rest) = t.strip_prefix("--") else {
            continue;
        };
        let Some(name) = rest.split(')').next() else {
            continue;
        };
        if name.is_empty() || !name.chars().all(|c| c.is_ascii_lowercase() || c == '-') {
            continue;
        }
        found += 1;
        let flag = &t[..name.len() + 2];
        if !header.contains(flag) {
            missing.push(flag);
        }
    }

    assert!(
        found >= 5,
        "only {found} mode(s) were read out of the parser — the shape this test reads has \
         changed, and a pin that matches nothing passes while proving nothing"
    );
    assert!(
        missing.is_empty(),
        "infra/gate.sh accepts {missing:?} but its usage header never names them. \
         A mode a reader cannot find is a mode nobody runs: --lint went unnamed there \
         and cost two gates to clippy errors it would have caught (packet 410e21e2)."
    );
}

/// `--quick` NAMES THE MODE THAT CLOSES THE GAP IT REPORTS.
///
/// `--quick` ends by stating, correctly, that clippy and the suites are
/// unproven. That is the moment a builder decides whether to push, and
/// stating a gap while withholding the remedy is what sent a gate after
/// a redundant closure. The line that names the gap must name `--lint`.
#[test]
fn the_quick_exit_names_the_mode_that_proves_clippy() {
    let gate = read("infra/gate.sh");
    let lines: Vec<&str> = gate.lines().collect();

    let quick_at = lines
        .iter()
        .position(|l| l.contains("if [ \"$QUICK\" -eq 1 ]; then"))
        .expect("infra/gate.sh no longer has a --quick early exit");
    let lint_at = lines
        .iter()
        .position(|l| l.contains("if [ \"$LINT\" -eq 1 ]; then"))
        .expect("infra/gate.sh no longer has a --lint branch");
    assert!(quick_at < lint_at, "the --quick branch precedes --lint");

    // Only what --quick itself PRINTS, so a comment mentioning --lint
    // somewhere in the file cannot satisfy this.
    let printed: Vec<&&str> = lines[quick_at..lint_at]
        .iter()
        .filter(|l| {
            let t = l.trim_start();
            t.starts_with("echo ") && !t.starts_with('#')
        })
        .collect();
    assert!(
        !printed.is_empty(),
        "the --quick branch prints nothing — this pin reads the wrong lines"
    );

    let unproven: Vec<&&&str> = printed.iter().filter(|l| l.contains("unproven")).collect();
    assert!(
        !unproven.is_empty(),
        "--quick no longer says what it leaves unproven; the honest edge of its claim \
         is the whole reason it is not a gate"
    );
    assert!(
        unproven.iter().any(|l| l.contains("--lint")),
        "--quick reports clippy as unproven without naming `--lint`, the mode that \
         proves it in seconds (packet 410e21e2). The lines it prints: {unproven:?}"
    );
}

/// THE DOORS LIST IS WHERE A SESSION LOOKS, so the door has to be there.
///
/// CLAUDE.md §Doors says a door that stops being true is a defect worth
/// a car. "Before pushing" named only `--quick` — true, but the half
/// that leaves clippy unproven — so every session read the cheaper door
/// and paid for the gap at the gate.
#[test]
fn the_doors_list_names_the_mode_that_proves_clippy() {
    let doc = read("CLAUDE.md");
    let door = doc
        .split("- **Before pushing")
        .nth(1)
        .and_then(|rest| rest.split("\n\n- **").next())
        .expect("CLAUDE.md §Doors no longer carries a `Before pushing` entry");
    assert!(
        door.contains("--lint"),
        "the `Before pushing` door names only the pre-flight. `infra/gate.sh --lint` is \
         the same pre-flight plus a scoped clippy, and clippy is the red class the door \
         exists to prevent (packet 410e21e2). The entry as written:\n{door}"
    );
}

/// `--lint` RUNS THE CHECK THE GATE'S FIRST ACT RUNS.
///
/// MEASURED on origin/main 1d917084, 2026-09-23 (backlog d8637703).
/// `scope_self_test` is build-free and was called on only two paths, `-p`
/// and `--auto` — the gate's own. Gate-run 1f412b9e went red before any
/// check ran, on `gate.sh scope self-test FAIL: a script boss-testing
/// executes implies boss-testing -> [boss-jobs boss-testing], wanted
/// [boss-testing]`, and no receipt was written. The branch had passed
/// `--lint` twice. A pre-flight that skips the check the gate runs first
/// vouches for a tree the gate refuses in its first second.
///
/// Read out of the `--lint` branch rather than run: `--lint` compiles
/// (clippy), and `scope_self_test` is silent on success, so a run could
/// not tell "held" from "never asked". The call must precede the scope
/// it vouches for — `crates_from_paths` is the map it tests.
#[test]
fn lint_runs_the_scope_self_test_before_the_scope_it_vouches_for() {
    let gate = read("infra/gate.sh");
    let lines: Vec<&str> = gate.lines().collect();
    let start = lines
        .iter()
        .position(|l| l.trim_end() == "if [ \"$LINT\" -eq 1 ]; then")
        .expect("infra/gate.sh no longer has a --lint branch");
    let len = lines[start..]
        .iter()
        .position(|l| *l == "fi")
        .expect("the --lint branch never closes at column 0");
    let branch = &lines[start..start + len];

    let scope = branch
        .iter()
        .position(|l| {
            let t = l.trim_start();
            !t.starts_with('#') && t.contains("crates_from_paths")
        })
        .expect("the --lint branch no longer derives its scope from crates_from_paths");
    match branch.iter().position(|l| l.trim() == "scope_self_test") {
        Some(at) => assert!(
            at < scope,
            "the --lint branch calls scope_self_test AFTER crates_from_paths — it derives \
             the clippy scope from a map it has not yet checked:\n{}",
            branch.join("\n")
        ),
        None => panic!(
            "the --lint branch never calls scope_self_test, so a stale scope fixture passes \
             the pre-flight and reds the gate before any check runs (gate-run 1f412b9e, \
             backlog d8637703). The branch reads:\n{}",
            branch.join("\n")
        ),
    }
}

/// EVERY FUNCTION IS DEFINED ABOVE ITS FIRST TOP-LEVEL CALLER.
///
/// bash defines a function when execution reaches its definition, so a
/// top-level line that calls one defined further down answers `command
/// not found` — and inside `$(...)` that is an empty string, not an
/// error, so the surrounding test simply goes the other way. MEASURED on
/// origin/main 1d917084 (backlog d8637703, reported by builder run
/// 2094f5d9): the `-p` refusal ran `$(schema_touched)` at :1167 and the
/// function was defined at :1207, so the refusal printed `schema_touched:
/// command not found` and silently dropped the line saying a schema
/// change widened the scope.
///
/// Read for EVERY function, not only that one, because the shape is the
/// file's and not the function's: the script is one long top-level
/// program with its functions defined inline, where the next one moved
/// or added is the next instance. Only column-0 definitions (`name() {`
/// through a column-0 `}`) and only call-shaped uses count — a statement
/// start, `$(`, `if`, `!`, `then`, `do`, or after `;`, `&`, `|` — so a
/// function's name inside a sentence an `echo` prints does not (`check`
/// is one: "nothing to check." is printed above `check()`).
#[test]
fn every_gate_function_is_defined_above_its_first_top_level_caller() {
    let gate = read("infra/gate.sh");
    let lines: Vec<&str> = gate.lines().collect();

    let def = regex::Regex::new(r"^([A-Za-z_][A-Za-z0-9_]*)\(\) \{").expect("definition regex");
    let mut defined: Vec<(String, usize)> = Vec::new();
    let mut in_body = vec![false; lines.len()];
    let mut i = 0;
    while i < lines.len() {
        let Some(name) = def.captures(lines[i]).map(|c| c[1].to_string()) else {
            i += 1;
            continue;
        };
        if !defined.iter().any(|(n, _)| *n == name) {
            defined.push((name, i));
        }
        let end = lines[i..]
            .iter()
            .position(|l| *l == "}")
            .map_or(lines.len() - 1, |n| i + n);
        in_body[i..=end].iter_mut().for_each(|b| *b = true);
        i = end + 1;
    }
    assert!(
        defined.len() >= 20,
        "only {} function definition(s) were read out of infra/gate.sh — the shape this \
         test reads has changed, and a pin that matches nothing passes while proving nothing",
        defined.len()
    );

    let mut called = 0usize;
    let mut early: Vec<String> = Vec::new();
    for (name, at) in &defined {
        let call = regex::Regex::new(&format!(
            r"(?:^|[;&|(]\s*|\bthen\s+|\bdo\s+|\bif\s+|!\s+){}(?:\s|$|\)|;)",
            regex::escape(name)
        ))
        .expect("call regex");
        let first = lines.iter().enumerate().find(|(k, l)| {
            let t = l.trim();
            !in_body[*k] && !t.starts_with('#') && call.is_match(t)
        });
        if let Some((k, l)) = first {
            called += 1;
            if k < *at {
                early.push(format!(
                    "{name}: called at line {}, defined at line {}: {}",
                    k + 1,
                    at + 1,
                    l.trim()
                ));
            }
        }
    }
    assert!(
        called >= 10,
        "only {called} function(s) were found called at top level — the call shape this \
         test reads has changed, and a pin that matches nothing passes while proving nothing"
    );
    assert!(
        early.is_empty(),
        "infra/gate.sh calls a function above its definition, which bash answers with \
         `command not found` (backlog d8637703):\n{}",
        early.join("\n")
    );
}

/// THE PRE-FLIGHT CHECKS THE TREE IT LIVES IN, SO IT REFUSES TO CLAIM ANOTHER.
///
/// Measured 2026-09-22 on one worktree, one commit, one second (backlog
/// 67adb415): `bash /work/boss/infra/gate.sh --lint` from a builder's
/// worktree printed "no crate implied by the tree - skipping clippy",
/// while `bash infra/gate.sh --lint` there said "clippy on boss-testing".
/// The script `cd`s to its own tree, so an absolute path asked every git
/// question of the clean main checkout — and then printed "pre-flight:
/// clean, and clippy saw the crates this tree changed", which is false
/// and is the sentence a builder reads.
///
/// So a caller standing in a DIFFERENT git tree is refused before any
/// check runs, and the refusal names the command that checks the
/// caller's tree. A caller inside the script's own tree — the gate
/// runner, CI, every other test in this file — is untouched.
#[test]
fn run_from_another_tree_the_pre_flight_refuses_rather_than_checking_its_own() {
    let caller = boss_testing::scratch_dir("gate-sh-other-tree");
    let init = std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(&caller)
        .status()
        .expect("git init");
    assert!(init.success(), "git init in {}", caller.display());

    let out = gate_cmd(&["--quick"])
        .current_dir(&caller)
        .output()
        .expect("run gate.sh");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(2),
        "gate.sh run by absolute path from another git tree must refuse (exit 2), \
         not check its own tree and call it yours. stderr:\n{stderr}"
    );
    let caller_real = std::fs::canonicalize(&caller).expect("canonicalize caller");
    assert!(
        stderr.contains(&caller_real.display().to_string())
            && stderr.contains("bash infra/gate.sh"),
        "the refusal names the caller's tree and the command that checks it:\n{stderr}"
    );
}
