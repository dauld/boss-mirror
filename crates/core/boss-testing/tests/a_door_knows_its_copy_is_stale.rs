//! Every pod door under `infra/dev/` is reached through a symlink in
//! `/work/tools/bin`, so it runs whatever the MAIN checkout's working
//! copy last held — and nothing keeps that checkout on `origin/main`.
//! CLAUDE.md §Doors asserts the opposite ("which is why the main
//! checkout stays on origin/main"); on 2026-09-19 it was two commits
//! behind for most of the working day, and a builder's first dogfood
//! of the new `BOSS_SOR_SERVICE` route through the pod's `boss-api`
//! answered `HTTP:404` from the jobs port while its own worktree copy
//! answered `HTTP:200`. A 404 from the wrong port is indistinguishable
//! from a surface that does not exist: CLAUDE.md's own "a wrong target
//! answers instead of erroring" (backlog 0b36dd65).
//!
//! `infra/dev/door-freshness.sh` is the one place that answers "is the
//! copy of this door that just ran the tree's copy?", and every door
//! sources it. What it pins here:
//!
//!   * STALE is measured LOCALLY and never fetches. A door is called
//!     constantly — a `git fetch` per call would be both slow and a
//!     new way to fail — so the comparison is against
//!     `refs/remotes/origin/main`, whatever the last fetch in any
//!     worktree of the clone left there (worktrees share remote
//!     refs). One git call answers the common case.
//!   * A DOOR BEING CHANGED IS NOT A STALE DOOR. Three questions must
//!     all say "behind" before a word is printed: HEAD is an ANCESTOR
//!     of origin/main (not merely different — a diverged branch is a
//!     developer's), the file on disk is HEAD's own version of it (not
//!     an edit in progress), and HEAD's version differs from
//!     origin/main's. The helper's first draft asked only the first
//!     two loosely and warned about its own author's worktree at
//!     17:38, one commit after origin/main moved.
//!   * THE SEVERITY SPLITS BY WHAT AN ANSWER COSTS. `boss-api` GET
//!     warns and answers — the warning rides beside the answer and the
//!     reader can judge it — while a WRITE is refused (exit 78, the
//!     shim's code for a door that is not the tree's), because what a
//!     write lands in the audit log is immutable and there is no
//!     reading it back "under a warning". The three doors that only
//!     build (`boss`, `wt-cargo`, `wt-web`) warn and proceed: refusing
//!     `boss gate` mid-build would strand a builder at the one moment
//!     it matters, which the shim's own history (backlog 49d9e99d)
//!     already records as the more expensive failure.
//!   * IT ANSWERS ABOUT THE FILE, NOT ABOUT `cwd`. The door is invoked
//!     through `/work/tools/bin/<name>` from inside a builder's
//!     worktree; the question is about the checkout the SYMLINK
//!     resolves into. Testing only the worktree's own copy is exactly
//!     what hid this defect for a day.

use boss_testing::{create_dir, repo_root, scratch_dir, write_exec, write_file};
use std::path::{Path, PathBuf};
use std::process::Command;

/// The doors the pod reaches through `/work/tools/bin`, and which of
/// them may refuse. Everything executable in `infra/dev/` must consult
/// the helper; this list says so by name so a NEW door cannot be added
/// without a decision about its severity.
const DOORS: &[&str] = &["boss-api", "boss", "wt-cargo", "wt-web"];

struct Pod {
    root: PathBuf,
    /// A git checkout holding a copy of `infra/dev/`.
    checkout: PathBuf,
    /// `<root>/bin`, holding the symlinks the pod reaches doors by,
    /// plus the stubs (`curl`, `cargo`) the doors would otherwise run.
    bin: PathBuf,
    home: PathBuf,
    /// Written by the `curl` stub, so a refusal can be shown to have
    /// happened BEFORE any request left.
    argv: PathBuf,
    /// The two commits: `old` is what the checkout holds, `main` is
    /// where `origin/main` stands.
    old: String,
    main: String,
}

struct Run {
    code: i32,
    stdout: String,
    stderr: String,
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .current_dir(dir)
        .args(["-c", "user.email=t@test", "-c", "user.name=test"])
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("git {args:?}: {e}"));
    assert!(
        out.status.success(),
        "git {args:?} in {}: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

impl Pod {
    /// A checkout two commits behind an `origin/main` that changed
    /// every door — the 2026-09-19 measurement, in miniature.
    fn new(name: &str) -> Self {
        let root = scratch_dir(&format!("door-stale-{name}"));
        let checkout = root.join("boss");
        let dev = checkout.join("infra/dev");
        create_dir(&dev);
        for f in ["door-freshness.sh", "pod-build.env"]
            .iter()
            .chain(DOORS.iter())
        {
            let src = repo_root().join("infra/dev").join(f);
            let body = std::fs::read_to_string(&src)
                .unwrap_or_else(|e| panic!("read {}: {e}", src.display()));
            if DOORS.contains(f) {
                write_exec(&dev.join(f), &body);
            } else {
                write_file(&dev.join(f), &body);
            }
        }
        write_file(&dev.join("sor-url"), "http://sor.test:7900\n");
        // boss-api's roll wait lives in infra/lib (backlog 834ddb7c),
        // and the door refuses before curl without it.
        let lib = checkout.join("infra/lib");
        create_dir(&lib);
        write_file(
            &lib.join("curl-through-a-roll.sh"),
            &std::fs::read_to_string(repo_root().join("infra/lib/curl-through-a-roll.sh"))
                .expect("read the roll lib"),
        );
        git(&checkout, &["init", "-q", "-b", "main"]);
        git(&checkout, &["add", "-A"]);
        git(&checkout, &["commit", "-qm", "doors"]);
        let old = git(&checkout, &["rev-parse", "HEAD"]);
        // origin/main: two commits on, each touching every door.
        for n in ["one", "two"] {
            for d in DOORS {
                let p = dev.join(d);
                let body = std::fs::read_to_string(&p).expect("read door");
                write_file(&p, &format!("{body}\n# landed on main: {n}\n"));
            }
            git(&checkout, &["add", "-A"]);
            git(&checkout, &["commit", "-qm", n]);
        }
        let main = git(&checkout, &["rev-parse", "HEAD"]);
        git(
            &checkout,
            &["update-ref", "refs/remotes/origin/main", &main],
        );
        git(&checkout, &["reset", "-q", "--hard", &old]);

        let bin = root.join("bin");
        create_dir(&bin);
        let home = root.join("home");
        create_dir(&home);
        let argv = root.join("curl-argv.txt");
        write_exec(
            &bin.join("curl"),
            "#!/usr/bin/env bash\n\
             : > \"$STUB_ARGV\"\n\
             for a in \"$@\"; do printf '%s\\n' \"$a\" >> \"$STUB_ARGV\"; done\n\
             printf '%s\\n%s' '{}' \"${STUB_CODE:-200}\"\n",
        );
        write_exec(
            &bin.join("cargo"),
            "#!/usr/bin/env bash\necho \"cargo $*\"\n",
        );
        // The pod's shape: the door is reached through a symlink into
        // the checkout, never as the file's own path.
        for d in DOORS {
            std::os::unix::fs::symlink(dev.join(d), bin.join(d))
                .unwrap_or_else(|e| panic!("link {d}: {e}"));
        }
        Self {
            root,
            checkout,
            bin,
            home,
            argv,
            old,
            main,
        }
    }

    /// Move the checkout onto `origin/main` — the hand fast-forward
    /// this packet is about being run.
    fn fast_forward(&self) {
        git(&self.checkout, &["reset", "-q", "--hard", &self.main]);
    }

    /// Run a door THROUGH THE SYMLINK, from a cwd that is not the
    /// door's checkout (a builder's worktree is another one).
    fn run(&self, door: &str, args: &[&str], env: &[(&str, &str)]) -> Run {
        let _ = std::fs::remove_file(&self.argv);
        let mut cmd = Command::new(self.bin.join(door));
        // wt-cargo and wt-web answer about the worktree they are RUN
        // in and refuse outside one; the checkout is the only git
        // worktree this fixture has.
        let cwd = if door.starts_with("wt-") {
            &self.checkout
        } else {
            &self.root
        };
        cmd.current_dir(cwd)
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    self.bin.display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env("HOME", &self.home)
            .env("STUB_ARGV", &self.argv)
            .env("BOSS_ACTOR", "agent:test")
            .env("BOSS_JOBS_URL", "http://sor.test:7900")
            .env("BOSS_MACHINE_TOKEN_FILE", self.root.join("no-token"))
            // The shim's own candidates come from the fixture, never
            // from the pod's store and /scratch: its BINARY check is
            // another mechanism (it refuses a write from a CLI behind
            // origin/main), and this test is about the shim FILE.
            .env("BOSS_SHIM_IMAGE_STORE", self.root.join("image-cli"))
            .env("BOSS_SHIM_TARGET_ROOT", self.root.join("no-target"))
            // …and the building doors stay off /scratch and out of the
            // operator's checkout, the way their own pins do.
            .env("WT_SEED", self.root.join("no-seed"))
            .env("WT_TARGET_ROOT", self.root.join("wt"))
            .env("WT_WEB_SOURCE", self.root.join("no-web-source"))
            .env_remove("BOSS_SOR_PORTS")
            .env_remove("BOSS_SOR_SERVICE")
            .env_remove("BOSS_DOOR_FRESHNESS")
            .args(args);
        for (k, v) in env {
            cmd.env(k, v);
        }
        let out = cmd.output().unwrap_or_else(|e| panic!("run {door}: {e}"));
        Run {
            code: out.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        }
    }

    fn curl_ran(&self) -> bool {
        self.argv.exists()
    }
}

/// Every executable in `infra/dev/` consults the one helper. A door
/// added without it is the defect again, and a comment asking the next
/// person to remember is not a mechanism (CLAUDE.md §9a).
#[test]
fn every_door_in_the_tree_consults_the_one_helper() {
    let dev = repo_root().join("infra/dev");
    let helper = dev.join("door-freshness.sh");
    assert!(helper.is_file(), "{} must exist", helper.display());
    let mut checked = 0;
    for entry in std::fs::read_dir(&dev).expect("read infra/dev") {
        let path = entry.expect("dir entry").path();
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        if !path.is_file() || name == "door-freshness.sh" {
            continue;
        }
        use std::os::unix::fs::PermissionsExt;
        if std::fs::metadata(&path).expect("stat").permissions().mode() & 0o111 == 0 {
            continue;
        }
        // as-gate-uid.sh is run BY a builder inside its own worktree to
        // rehearse the gate's uid; it answers about the workspace it is
        // handed, not from the main checkout, and it is not on the pod's
        // PATH as a door.
        if name == "as-gate-uid.sh" {
            continue;
        }
        let body = std::fs::read_to_string(&path).expect("read door");
        assert!(
            body.contains("door-freshness.sh"),
            "infra/dev/{name} is a door on the pod's PATH and does not source \
             door-freshness.sh: reached through /work/tools/bin it runs whatever the main \
             checkout last had, and answers from a stale copy without saying so \
             (backlog 0b36dd65)"
        );
        checked += 1;
    }
    assert!(
        checked >= DOORS.len(),
        "only {checked} doors checked; {DOORS:?} are all doors"
    );
}

#[test]
fn a_stale_read_answers_under_a_warning_naming_both_shas_and_the_fix() {
    let pod = Pod::new("read");
    let run = pod.run("boss-api", &["GET", "/api/jobs"], &[]);
    assert_eq!(
        run.code, 0,
        "a stale READ must still answer: {}",
        run.stderr
    );
    assert!(
        pod.curl_ran(),
        "the read did not reach curl: {}",
        run.stderr
    );
    assert!(run.stdout.contains("{}"), "body: {:?}", run.stdout);
    let e = &run.stderr;
    assert!(
        e.contains("WARNING"),
        "a stale door answered without a warning — the well-formed wrong answer this \
         packet is about: {e}"
    );
    assert!(
        e.contains(&pod.old[..8]) && e.contains(&pod.main[..8]),
        "the warning names neither sha, so a reader re-derives what the door already \
         knows (CLAUDE.md §Diagnosis): {e}"
    );
    assert!(
        e.contains("2 commits behind"),
        "the warning does not say HOW far behind: {e}"
    );
    assert!(
        e.contains(&format!(
            "git -C {} merge --ff-only origin/main",
            pod.checkout.display()
        )),
        "the warning does not carry the exact command that repairs it: {e}"
    );
    assert!(
        e.contains("boss-api"),
        "the warning does not name the door it is about: {e}"
    );
}

#[test]
fn a_stale_write_is_refused_before_it_reaches_the_record() {
    let pod = Pod::new("write");
    let body = pod.root.join("body.json");
    write_file(&body, "{}\n");
    for method in ["POST", "PUT", "PATCH", "DELETE"] {
        let run = pod.run(
            "boss-api",
            &[method, "/api/jobs", body.to_str().unwrap()],
            &[],
        );
        assert_eq!(
            run.code, 78,
            "a stale {method} exited {} — a write from a door that is not the tree's lands \
             an immutable fact: {}",
            run.code, run.stderr
        );
        assert!(
            !pod.curl_ran(),
            "the stale {method} reached curl before being refused: {}",
            run.stderr
        );
        assert!(
            run.stderr.contains("REFUSED") && run.stderr.contains("2 commits behind"),
            "the refusal does not say what it refused and why: {}",
            run.stderr
        );
        assert!(
            run.stderr.contains("BOSS_DOOR_FRESHNESS=off"),
            "the refusal names no way through for someone who means it: {}",
            run.stderr
        );
    }
}

#[test]
fn a_checkout_on_origin_main_says_nothing_at_all() {
    let pod = Pod::new("fresh");
    pod.fast_forward();
    let read = pod.run("boss-api", &["GET", "/api/jobs"], &[]);
    assert_eq!(read.code, 0, "{}", read.stderr);
    assert!(
        !read.stderr.contains("WARNING") && !read.stderr.contains("behind"),
        "a current door is not quiet — a warning nobody can act on is read as noise and \
         then the real one is too: {}",
        read.stderr
    );
    let body = pod.root.join("body.json");
    write_file(&body, "{}\n");
    let write = pod.run(
        "boss-api",
        &["POST", "/api/jobs", body.to_str().unwrap()],
        &[],
    );
    assert_eq!(
        write.code, 0,
        "a current write is refused: {}",
        write.stderr
    );
    assert!(
        pod.curl_ran(),
        "the write did not reach curl: {}",
        write.stderr
    );
}

/// A door being CHANGED is not a stale door — this very car was
/// written that way, and the helper's first draft warned about itself
/// at 17:38 (origin/main had moved one commit while the branch was
/// still uncommitted). Two shapes of the same thing: an uncommitted
/// edit in a checkout that is behind, and a branch that has diverged.
/// Both must be silent, or every builder worktree warns about itself
/// every time a train lands and the warning stops being read.
#[test]
fn a_door_being_changed_is_not_a_stale_door() {
    for shape in ["uncommitted", "committed"] {
        let pod = Pod::new(&format!("changed-{shape}"));
        let door = pod.checkout.join("infra/dev/boss-api");
        let body = std::fs::read_to_string(&door).expect("read door");
        write_file(&door, &format!("{body}\n# an edit in progress\n"));
        if shape == "committed" {
            git(&pod.checkout, &["add", "-A"]);
            git(&pod.checkout, &["commit", "-qm", "the builder's own car"]);
        }
        let run = pod.run("boss-api", &["GET", "/api/jobs"], &[]);
        assert_eq!(run.code, 0, "{}", run.stderr);
        assert!(
            !run.stderr.contains("WARNING"),
            "a door being changed ({shape}) warned about itself: {}",
            run.stderr
        );
    }
    // And on a checkout standing exactly on origin/main, nothing.
    let pod = Pod::new("edited-current");
    pod.fast_forward();
    let door = pod.checkout.join("infra/dev/boss-api");
    let body = std::fs::read_to_string(&door).expect("read door");
    write_file(&door, &format!("{body}\n# an edit in progress\n"));
    let run = pod.run("boss-api", &["GET", "/api/jobs"], &[]);
    assert!(
        !run.stderr.contains("WARNING"),
        "an edited door on a current checkout warned about itself: {}",
        run.stderr
    );
}

/// The escape hatch, and the reason it exists: a door must never be
/// the thing that cannot be got past.
#[test]
fn the_override_runs_a_stale_write_anyway() {
    let pod = Pod::new("override");
    let body = pod.root.join("body.json");
    write_file(&body, "{}\n");
    let run = pod.run(
        "boss-api",
        &["POST", "/api/jobs", body.to_str().unwrap()],
        &[("BOSS_DOOR_FRESHNESS", "off")],
    );
    assert_eq!(
        run.code, 0,
        "the override did not let the write through: {}",
        run.stderr
    );
    assert!(pod.curl_ran(), "no request left: {}", run.stderr);
}

/// Outside a git checkout — the gate's fetched workspace, a copy on a
/// host with no clone — the question cannot be answered, and an
/// unanswerable question is silence, never a refusal: visibility is
/// best-effort (CLAUDE.md §Diagnosis), and a door that refuses because
/// it cannot see is worse than the stale answer it prevents.
#[test]
fn a_copy_with_no_checkout_behind_it_runs_silently() {
    let pod = Pod::new("nogit");
    let loose = pod.root.join("loose");
    create_dir(&loose);
    for f in ["door-freshness.sh", "boss-api", "sor-url"] {
        let body =
            std::fs::read_to_string(pod.checkout.join("infra/dev").join(f)).expect("read door");
        if f == "sor-url" {
            write_file(&loose.join(f), &body);
        } else {
            write_exec(&loose.join(f), &body);
        }
    }
    // The roll wait, where the loose copy looks for it: ../lib.
    let lib = pod.root.join("lib");
    create_dir(&lib);
    write_file(
        &lib.join("curl-through-a-roll.sh"),
        &std::fs::read_to_string(pod.checkout.join("infra/lib/curl-through-a-roll.sh"))
            .expect("read the roll lib"),
    );
    let out = Command::new(loose.join("boss-api"))
        .current_dir(&loose)
        .env(
            "PATH",
            format!(
                "{}:{}",
                pod.bin.display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        .env("HOME", &pod.home)
        .env("STUB_ARGV", &pod.argv)
        .env("BOSS_ACTOR", "agent:test")
        .env("BOSS_JOBS_URL", "http://sor.test:7900")
        .env("BOSS_MACHINE_TOKEN_FILE", pod.root.join("no-token"))
        .args(["GET", "/api/jobs"])
        .output()
        .expect("run loose boss-api");
    assert_eq!(
        out.status.code().unwrap_or(-1),
        0,
        "a copy with no checkout behind it did not answer: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let e = String::from_utf8_lossy(&out.stderr);
    assert!(
        !e.contains("WARNING") && !e.contains("REFUSED"),
        "an unanswerable freshness question spoke anyway: {e}"
    );
}

/// The three building doors warn and PROCEED — for both a read verb
/// and a write one. `boss gate` is a write, and a builder refused at
/// its gate launch is the failure the shim's own history already
/// counted five times in one day (backlog 49d9e99d).
#[test]
fn the_building_doors_warn_and_never_refuse() {
    let pod = Pod::new("building");
    for (door, args) in [
        ("boss", vec!["--version"]),
        ("boss", vec!["gate", "some-branch"]),
        ("wt-cargo", vec!["check"]),
        ("wt-web", vec![]),
    ] {
        let run = pod.run(door, &args, &[]);
        assert!(
            run.stderr.contains("WARNING") && run.stderr.contains("2 commits behind"),
            "{door} {args:?} ran from a stale copy without saying so: {}",
            run.stderr
        );
        assert_ne!(
            run.code, 78,
            "{door} {args:?} REFUSED over its own file's age; a builder refused at its gate \
             launch is worse than the stale answer: {}",
            run.stderr
        );
    }
}

/// ONE ESCAPE, TWO LANGUAGES (CLAUDE.md §9a, backlog 6f581de6).
///
/// The shell helper above and `crates/orchestrators/boss-cli/src/freshness.rs`
/// ask deliberately DIFFERENT questions and that difference is the
/// design, not a drift: the helper asks whether this checkout is an
/// ancestor of `origin/main` and stays quiet on a branch, because a
/// branch is a developer working; the Rust guard asks whether
/// `origin/main` is an ancestor of HEAD, so a branch cut from an old
/// main IS judged, because a recorded probe reads that old tree with
/// `git show HEAD:` either way.
///
/// What DOES live twice is the escape. An operator who silences one
/// door means to silence the freshness question, and finding that the
/// other door answers to a different spelling is the wrong-target
/// failure this whole area exists to prevent. A string in a `.sh` and a
/// `const` in a `.rs` cannot be collapsed into one definition, so §9a's
/// other half applies: pin it, and name the offending file when it
/// drifts.
#[test]
fn both_freshness_doors_answer_to_one_env_name() {
    const ESCAPE: &str = "BOSS_DOOR_FRESHNESS";
    let sh_path = repo_root().join("infra/dev/door-freshness.sh");
    let rs_path = repo_root().join("crates/orchestrators/boss-cli/src/freshness.rs");
    let sh = std::fs::read_to_string(&sh_path).expect("read the shell helper");
    let rs = std::fs::read_to_string(&rs_path).expect("read the Rust guard");

    assert!(
        sh.contains(&format!("[ \"${{{ESCAPE}:-}}\" = off ]")),
        "{} no longer silences on {ESCAPE}=off; the Rust guard in {} still does, so \
         silencing one door leaves the other talking",
        sh_path.display(),
        rs_path.display(),
    );
    assert!(
        rs.contains(&format!("FRESHNESS_ENV: &str = \"{ESCAPE}\"")),
        "{} no longer answers to {ESCAPE}, which {} still silences on",
        rs_path.display(),
        sh_path.display(),
    );
    assert!(
        rs.contains("v.trim() == \"off\""),
        "{} must silence on the same VALUE the shell helper does — `off`, not `1` or \
         `true`: an escape spelled two ways is an escape that half works",
        rs_path.display(),
    );
    // And the Rust side spells the name exactly once, so every reader
    // there goes through the const: one side of a pinned pair that has
    // its own internal copies is a pair with three members.
    assert_eq!(
        rs.matches(&format!("\"{ESCAPE}\"")).count(),
        1,
        "{} spells the escape as a literal more than once; every reader takes \
         FRESHNESS_ENV, which cannot drift from itself",
        rs_path.display(),
    );
}
