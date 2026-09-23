//! `infra/cluster/dev-scratch-reclaim.sh` — the dev pod's reclaim
//! sidecar — reclaims the per-builder target dirs that actually
//! accumulate, not only the one path it was told about.
//!
//! Measured 2026-09-11 01:40Z (backlog 0efbd69e): /scratch held 90 GB,
//! of which /scratch/target — the ONLY dir the reclaim knew — was 28 GB
//! and twenty sibling dirs (target-item-at-open, target-ae887a1b,
//! tgt-rerail-sweep, …) held 62 GB, nearly all for branches that had
//! landed the day before. Every coding agent is briefed to use its own
//! CARGO_TARGET_DIR under /scratch, so the caches that pile up are
//! SIBLINGS of the path the reclaim scanned, and the reclaim could not
//! see one byte of them. Its floor (50 GB free) would not have fired
//! anyway with 304 GB free — a dead cache is not a headroom problem, it
//! is an AGE problem, so the pass below runs on age regardless of free
//! space. Driven as a real script against a fixture scratch mount.

use boss_testing::repo_root;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// Removes its directory on drop, so a panicking test leaves nothing.
struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn touch_at(path: &Path, hours_ago: u64) {
    let when = format!("-{hours_ago} hours");
    let ok = Command::new("touch")
        .args(["-d", &when])
        .arg(path)
        .status()
        .expect("touch")
        .success();
    assert!(ok, "touch -d {when} {}", path.display());
}

/// A dir shaped like a cargo target: `CACHEDIR.TAG` plus `debug/`, with
/// every mtime set `hours_ago`.
fn target_dir(root: &Path, name: &str, hours_ago: u64) -> PathBuf {
    let d = root.join(name);
    boss_testing::create_dir(&d.join("debug").join("deps"));
    boss_testing::write_file(
        &d.join("CACHEDIR.TAG"),
        "Signature: 8a477f597d28d172789f06886806bc55\n",
    );
    boss_testing::write_file(&d.join("debug").join("deps").join("libfoo.rlib"), "x");
    for p in [
        d.join("debug").join("deps").join("libfoo.rlib"),
        d.join("debug").join("deps"),
        d.join("debug"),
        d.join("CACHEDIR.TAG"),
        d.clone(),
    ] {
        touch_at(&p, hours_ago);
    }
    d
}

/// A stub `curl` on PATH, so a pass that files its packet reaches this
/// and never a network: every call is appended to `curl-log.txt` as
/// `METHOD URL BODY`, and the answers walk the wrap + boss-step
/// sequence the real system of record would — the first open-packet
/// GET finds none, the POST opens `job-1`, later GETs find it with a
/// ready `run` step, a PUT is acknowledged. The maintenance helpers
/// parse the reply shape they always did; only the transport is a
/// fake.
fn stub_curl(root: &Path) -> PathBuf {
    let bin = root.join("bin");
    boss_testing::create_dir(&bin);
    boss_testing::write_exec(
        &bin.join("curl"),
        concat!(
            "#!/usr/bin/env bash\n",
            "method=GET; url=; body=\n",
            "while [ $# -gt 0 ]; do\n",
            "    case \"$1\" in\n",
            "        -X) method=$2; shift ;;\n",
            "        -d) body=$(printf '%s' \"$2\" | jq -c . 2>/dev/null || printf '%s' \"$2\"); shift ;;\n",
            "        -H) shift ;;\n",
            "        http*) url=$1 ;;\n",
            "    esac\n",
            "    shift\n",
            "done\n",
            "printf '%s %s %s\\n' \"$method\" \"$url\" \"$body\" >> \"$STUB_LOG\"\n",
            // The fast-forward pass asks ONE question of the system of
            // record — is a gate LAUNCHING right now — and a test
            // decides the answer: none open by default; one open with
            // STUB_OPEN_GATE, aged by STUB_GATE_AGE_SECS (default 0, a
            // gate opened this second); a page that reports more open
            // runs than it returns with STUB_GATE_TOTAL; and an API
            // that ANSWERS an error (curl's 22 under -f) with
            // STUB_GATE_RC.
            "case \"$url\" in\n",
            "    *kind=gate-run*)\n",
            "        if [ -n \"${STUB_GATE_RC:-}\" ]; then exit \"$STUB_GATE_RC\"; fi\n",
            "        if [ -n \"${STUB_OPEN_GATE:-}\" ]; then\n",
            "            now=$(date -u +%s)\n",
            "            at=$(date -u -d \"@$((now - ${STUB_GATE_AGE_SECS:-0}))\" +%Y-%m-%dT%H:%M:%S.000000000+00:00)\n",
            "            jq -nc --arg at \"$at\" --argjson total \"${STUB_GATE_TOTAL:-1}\" \\\n",
            "               '{total: $total, data: [{id: \"gate-1\", status: \"open\", metadata: {opened_at: $at}}]}'\n",
            "        else echo '{\"total\":0,\"data\":[]}'; fi\n",
            "        exit 0 ;;\n",
            "esac\n",
            "case \"$method\" in\n",
            "    POST) touch \"$STUB_LOG.opened\"; echo '{\"id\":\"job-1\"}' ;;\n",
            "    PUT) echo '{}' ;;\n",
            "    GET) if [ -e \"$STUB_LOG.opened\" ]; then\n",
            "             echo '{\"data\":[{\"id\":\"job-1\",\"status\":\"open\",\"steps\":[",
            "{\"id\":\"step-run\",\"spec_slug\":\"run\",\"status\":\"ready\",\"metadata\":{}}]}]}'\n",
            "         else echo '{\"data\":[]}'; fi ;;\n",
            "esac\n",
        ),
    );
    bin
}

/// A `git` on PATH ahead of the real one that REFUSES `ls-remote` the
/// way the sidecar's does (backlog b50a65ef, 2026-09-18: the sidecar
/// mounts only /work and /scratch — no HOME, no credential helper — and
/// the forge answers an anonymous info/refs with 401, so `git ls-remote
/// --heads origin` there is `fatal: could not read Username`, exit 128).
/// Every other git command goes through to the real binary. A call that
/// reached ls-remote leaves `git-ls-remote.txt` behind for the test to
/// find.
fn stub_git(root: &Path) -> PathBuf {
    let bin = root.join("bin");
    boss_testing::create_dir(&bin);
    let real = String::from_utf8(
        Command::new("bash")
            .args(["-c", "command -v git"])
            .output()
            .expect("locate git")
            .stdout,
    )
    .expect("utf8");
    let real = real.trim();
    assert!(!real.is_empty(), "a real git on PATH");
    boss_testing::write_exec(
        &bin.join("git"),
        &format!(
            concat!(
                "#!/usr/bin/env bash\n",
                "for a in \"$@\"; do\n",
                "    if [ \"$a\" = ls-remote ]; then\n",
                "        printf '%s\\n' \"$*\" >> \"$STUB_LS_REMOTE\"\n",
                "        echo \"fatal: could not read Username for 'http://forge.test': terminal prompts disabled\" >&2\n",
                "        exit 128\n",
                "    fi\n",
                "done\n",
                "exec {real} \"$@\"\n",
            ),
            real = real
        ),
    );
    bin
}

/// What the stub git saw of ls-remote: empty when the pass never asked.
fn ls_remote_calls(scratch: &Path) -> String {
    std::fs::read_to_string(scratch.join("git-ls-remote.txt")).unwrap_or_default()
}

fn run(scratch: &Path, extra: &[(&str, &str)]) -> Output {
    let bin = stub_curl(scratch);
    stub_git(scratch);
    let installer = stub_installer(scratch);
    let mut cmd = Command::new("bash");
    cmd.arg(repo_root().join("infra/cluster/dev-scratch-reclaim.sh"))
        // The CLI leg goes to the stub installer below, never to the
        // registry; the stub's log is what the CLI tests read.
        .env("BOSS_CLI_INSTALLER", &installer)
        .env("STUB_INSTALL_LOG", scratch.join("install-log.txt"))
        .env("SCRATCH_MOUNT", scratch)
        .env("CARGO_TARGET_DIR", scratch.join("target"))
        .env("WORK_MOUNT", scratch.join("work"))
        .env("REPO_DIR", scratch.join("work").join("repo"))
        .env("WORKTREES_DIR", scratch.join("work").join("wt"))
        .env("BOSS_STALE_TARGET_H", "12")
        // The packet goes to the stub above, never to a real system of
        // record, and a transport failure gives up at once.
        .env("BOSS_JOBS_URL", "http://sor.test:7900")
        .env("BOSS_API_RETRY_DEADLINE", "0")
        .env("STUB_LOG", scratch.join("curl-log.txt"))
        .env("STUB_LS_REMOTE", scratch.join("git-ls-remote.txt"))
        .env(
            "PATH",
            format!(
                "{}:{}",
                bin.display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        );
    for (k, v) in extra {
        cmd.env(k, v);
    }
    cmd.output().expect("run dev-scratch-reclaim.sh")
}

/// What the stub curl saw, one `METHOD URL BODY` line per call.
fn curl_log(scratch: &Path) -> String {
    std::fs::read_to_string(scratch.join("curl-log.txt")).unwrap_or_default()
}

fn say(out: &Output) -> String {
    format!(
        "status {:?}\nstdout:\n{}\nstderr:\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[test]
fn a_stale_sibling_target_is_reclaimed_and_the_fresh_ones_and_the_primary_are_kept() {
    let root = boss_testing::scratch_dir("boss-dsr-siblings");
    let _guard = Scratch(root.clone());
    boss_testing::create_dir(&root.join("work").join("wt"));
    boss_testing::create_dir(&root.join("work").join("repo"));
    let primary = target_dir(&root, "target", 72);
    let stale = target_dir(&root, "target-landed-yesterday", 30);
    let fresh = target_dir(&root, "target-live-builder", 1);
    // Not a target at all — old, but not ours to touch.
    let notes = root.join("notes");
    boss_testing::create_dir(&notes);
    boss_testing::write_file(&notes.join("a.txt"), "keep");
    touch_at(&notes.join("a.txt"), 200);
    touch_at(&notes, 200);

    let out = run(&root, &[]);
    let text = say(&out);
    assert!(
        !stale.exists(),
        "the 30h-old sibling target must be reclaimed\n{text}"
    );
    assert!(
        fresh.exists(),
        "a sibling touched 1h ago belongs to a live builder\n{text}"
    );
    assert!(
        primary.exists(),
        "the primary CARGO_TARGET_DIR is never reclaimed by age\n{text}"
    );
    assert!(
        notes.join("a.txt").exists(),
        "a dir that is not a cargo target is not touched\n{text}"
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("target-landed-yesterday") && stdout.contains("stale target"),
        "the reclaim must NAME the dir it removed and why\n{text}"
    );
}

#[test]
fn nothing_stale_reclaims_nothing_and_says_so() {
    let root = boss_testing::scratch_dir("boss-dsr-quiet");
    let _guard = Scratch(root.clone());
    boss_testing::create_dir(&root.join("work").join("wt"));
    boss_testing::create_dir(&root.join("work").join("repo"));
    let fresh = target_dir(&root, "target-live", 2);
    let out = run(&root, &[]);
    let text = say(&out);
    assert!(fresh.exists(), "{text}");
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("0 stale target"),
        "a quiet pass still reports its count\n{text}"
    );
    // A pass that reclaimed nothing is a reading, not work: it files no
    // packet (David, 2026-09-16 — telemetry lives outside the audit
    // log; a chore turns a reading into a packet when it matters).
    assert!(
        curl_log(&root).is_empty(),
        "a quiet pass files nothing\n{}\n{text}",
        curl_log(&root)
    );
}

#[test]
fn the_age_threshold_is_validated_like_the_floors() {
    let root = boss_testing::scratch_dir("boss-dsr-badage");
    let _guard = Scratch(root.clone());
    let out = run(&root, &[("BOSS_STALE_TARGET_H", "soon")]);
    assert_eq!(out.status.code(), Some(64), "{}", say(&out));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("STALE_TARGET_H"),
        "{}",
        say(&out)
    );
}

// ---------------------------------------------------------------------
// The worktree pass (backlog 1933db9e, audit H11).
//
// Measured 2026-09-18 on the pod: 203 worktrees under /work/boss (20 on
// a detached HEAD), 182 distinct branches of which 175 no longer exist
// on the forge, 36 per-worktree target dirs on /scratch — 364 GB on a
// 929 GB disk at 77%. The stale-target pass above retires a target only
// by age and nothing ever retired a worktree, so a landed branch's
// checkout and its target both outlived it indefinitely. The pass under
// test removes a worktree whose branch the forge no longer has, when its
// tree is clean and it has had no git activity for a grace period, and
// takes the target wt-cargo named for it; a detached or main worktree
// is judged by idleness alone; anything dirty, live, recent or locked
// is kept and named.
//
// Driven against a REAL forge stand-in: a bare repository that `origin`
// points at, so a push leaves the checkout the `refs/remotes/origin/<b>`
// ref the pass reads. The pass reads ONLY those refs, never the forge
// (backlog b50a65ef, 2026-09-18): the sidecar has no forge credential,
// so the `git ls-remote --heads origin` the pass first shipped with
// (H11) answered `fatal: could not read Username` every hour and the
// three newest packets on the system of record all said
// `worktree_pass=skipped` with no reason. `run()` puts a git on PATH
// that refuses ls-remote, so a pass that reached for the network fails
// here the way it failed there.
// ---------------------------------------------------------------------

/// Run git in `dir`, with the author/committer fixed so no test reads the
/// pod's global config, and `hours_ago` applied to both dates so a
/// commit's age is what the test says it is.
fn git(dir: &Path, hours_ago: u64, args: &[&str]) -> String {
    let when = format!("{} +0000", unix_now() - hours_ago * 3600);
    let out = Command::new("git")
        .current_dir(dir)
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@test",
            "-c",
            "init.defaultBranch=main",
        ])
        .args(args)
        .env("GIT_AUTHOR_DATE", &when)
        .env("GIT_COMMITTER_DATE", &when)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .expect("run git");
    assert!(
        out.status.success(),
        "git {args:?} in {}:\n{}{}",
        dir.display(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_secs()
}

/// The pod's checkout in miniature: a bare `forge.git` the main
/// checkout's `origin` points at, the checkout itself at `work/repo`,
/// worktrees under `work/wt/<name>`.
struct Yard {
    root: PathBuf,
    repo: PathBuf,
}

impl Yard {
    fn new(root: &Path) -> Self {
        let forge = root.join("forge.git");
        boss_testing::create_dir(&forge);
        git(&forge, 0, &["init", "--bare", "-q"]);
        let repo = root.join("work").join("repo");
        boss_testing::create_dir(&repo);
        boss_testing::create_dir(&root.join("work").join("wt"));
        git(&repo, 100, &["init", "-q"]);
        boss_testing::write_file(&repo.join("README"), "seed\n");
        git(&repo, 100, &["add", "."]);
        git(&repo, 100, &["commit", "-q", "-m", "seed"]);
        git(
            &repo,
            100,
            &["remote", "add", "origin", forge.to_str().expect("utf8")],
        );
        git(&repo, 100, &["push", "-q", "origin", "main"]);
        Self {
            root: root.to_path_buf(),
            repo,
        }
    }

    /// A worktree at `work/wt/<name>` on a new branch (or detached when
    /// `branch` is None) with one commit of its own, every activity
    /// signal — the commit, the worktree's HEAD, index and reflog, the
    /// directory itself — dated `hours_ago`.
    fn worktree(&self, name: &str, branch: Option<&str>, hours_ago: u64) -> PathBuf {
        let path = self.root.join("work").join("wt").join(name);
        let p = path.to_str().expect("utf8");
        match branch {
            Some(b) => git(
                &self.repo,
                hours_ago,
                &["worktree", "add", "-q", "-b", b, p, "main"],
            ),
            None => git(
                &self.repo,
                hours_ago,
                &["worktree", "add", "-q", "--detach", p, "main"],
            ),
        };
        boss_testing::write_file(&path.join("work.txt"), name);
        git(&path, hours_ago, &["add", "."]);
        git(&path, hours_ago, &["commit", "-q", "-m", name]);
        let gitdir = PathBuf::from(git(&path, hours_ago, &["rev-parse", "--absolute-git-dir"]));
        for f in ["HEAD", "index", "logs/HEAD"] {
            let f = gitdir.join(f);
            if f.exists() {
                touch_at(&f, hours_ago);
            }
        }
        touch_at(&gitdir, hours_ago);
        touch_at(&path.join("work.txt"), hours_ago);
        touch_at(&path, hours_ago);
        path
    }

    /// Publish a branch to the forge stand-in — which leaves the
    /// checkout `refs/remotes/origin/<branch>`, the ref the pass reads.
    fn publish(&self, branch: &str) {
        git(&self.repo, 0, &["push", "-q", "origin", branch]);
    }

    /// Land a branch: its commit becomes the forge's `main`, so the
    /// checkout's `refs/remotes/origin/main` moves to it while NO
    /// `refs/remotes/origin/<branch>` is ever made — a car that merged
    /// and whose branch the forge swept.
    fn land(&self, branch: &str) {
        git(
            &self.repo,
            0,
            &["push", "-q", "origin", &format!("{branch}:main")],
        );
    }

    fn origin_main(&self) -> String {
        git(&self.repo, 0, &["rev-parse", "refs/remotes/origin/main"])
    }
}

#[test]
fn a_clean_worktree_whose_branch_is_gone_from_the_forge_is_removed_with_its_target() {
    let root = boss_testing::scratch_dir("boss-dsr-worktrees");
    let _guard = Scratch(root.clone());
    let yard = Yard::new(&root);

    // The bulk case: LANDED — its head is on origin/main, the forge
    // swept the branch so the checkout has no origin/ ref for it — and
    // idle past the grace window. Its target is FRESH so the age pass
    // cannot be what takes it.
    let gone = yard.worktree("agent-gone", Some("feat/gone"), 30);
    yard.land("feat/gone");
    let gone_target = target_dir(&root, "target-agent-gone", 1);
    // Still on the forge — the checkout has its origin/ ref — a car in
    // flight or parked. Kept whatever its age.
    let live = yard.worktree("agent-live", Some("feat/live"), 200);
    yard.publish("feat/live");
    let live_target = target_dir(&root, "target-agent-live", 1);
    // ABANDONED: never pushed (no origin/ ref, head not on origin/main)
    // and idle past WORKTREE_MAX_AGE_H — the harness's own
    // `worktree-agent-*` branches never reach the forge at all.
    let abandoned = yard.worktree("agent-abandoned", Some("feat/abandoned"), 60);
    // Never pushed but inside WORKTREE_MAX_AGE_H: a builder's branch is
    // absent from the forge until its push, so an unpushed tree gets
    // the longer window, not the grace one.
    let unpushed = yard.worktree("agent-unpushed", Some("feat/unpushed"), 30);
    // Abandoned, idle, but carrying uncommitted work: kept and named.
    // The edits are as old as the checkout — a tree edited minutes ago
    // is kept as RECENT before its dirtiness is even read.
    let dirty = yard.worktree("agent-dirty", Some("feat/dirty"), 60);
    boss_testing::write_file(&dirty.join("work.txt"), "edited, not committed");
    boss_testing::write_file(&dirty.join("notes.txt"), "untracked");
    for p in [
        dirty.join("work.txt"),
        dirty.join("notes.txt"),
        dirty.clone(),
    ] {
        touch_at(&p, 60);
    }
    // Landed but touched within the grace window.
    let recent = yard.worktree("agent-recent", Some("feat/recent"), 1);
    // No branch at all: judged by idleness, and 7 days is the bar.
    let detached_old = yard.worktree("agent-detached-old", None, 24 * 10);
    // ...and whose head a ref still holds, so removing the checkout
    // loses no commit (the orphan case is its own test below).
    let detached_old_head = git(&detached_old, 0, &["rev-parse", "HEAD"]);
    git(
        &yard.repo,
        0,
        &["update-ref", "refs/pulls/1", &detached_old_head],
    );
    let detached_new = yard.worktree("agent-detached-new", None, 24 * 2);
    // Its directory is already gone: only the admin entry remains.
    let vanished = yard.worktree("agent-vanished", Some("feat/vanished"), 30);
    std::fs::remove_dir_all(&vanished).expect("rm vanished worktree");

    let out = run(&root, &[]);
    let text = say(&out);
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert!(!gone.exists(), "gone branch, clean, idle: removed\n{text}");
    assert!(
        !gone_target.exists(),
        "and its wt-cargo target with it\n{text}"
    );
    assert!(
        live.exists(),
        "a branch the checkout still has an origin/ ref for is kept\n{text}"
    );
    assert!(live_target.exists(), "and so is its target\n{text}");
    assert!(
        !abandoned.exists(),
        "no origin/ ref, not on origin/main, idle past MAX_AGE_H: abandoned, removed\n{text}"
    );
    assert!(
        unpushed.exists(),
        "no origin/ ref, not on origin/main, inside MAX_AGE_H: a builder not yet pushed, kept\n{text}"
    );
    assert!(dirty.exists(), "a dirty tree is never removed\n{text}");
    assert!(
        stdout.contains("agent-dirty") && stdout.contains("2 dirty"),
        "the kept dirty tree is NAMED with its count\n{text}"
    );
    assert!(
        recent.exists(),
        "activity inside the grace window means in use\n{text}"
    );
    assert!(
        !detached_old.exists(),
        "a detached HEAD idle 10 days is removed\n{text}"
    );
    assert!(
        detached_new.exists(),
        "a detached HEAD idle 2 days is not\n{text}"
    );
    assert!(
        stdout.contains("worktree pass: removed 3"),
        "the pass reports its totals on one line\n{text}"
    );
    assert!(
        ls_remote_calls(&root).is_empty(),
        "the pass never reads the forge — the sidecar cannot\n{}\n{text}",
        ls_remote_calls(&root)
    );
    let main = yard.origin_main();
    assert!(
        stdout.contains(&main[..8]) && stdout.contains("last fetch"),
        "the pass names the origin/main it read and that it is only as fresh as the last fetch\n{text}"
    );
    let listed = git(&yard.repo, 0, &["worktree", "list", "--porcelain"]);
    assert!(
        !listed.contains("agent-vanished"),
        "an entry whose directory is gone is pruned\n{listed}\n{text}"
    );

    // The record: one packet opened, its run step completed with the
    // totals, so the reclaim is readable from the system of record and
    // not only from a sidecar log nobody tails.
    let log = curl_log(&root);
    assert!(
        log.contains("POST http://sor.test:7900/api/jobs ")
            && log.contains("\"kind\":\"maintenance-dev-scratch-reclaim\""),
        "a pass that acted opens its packet\n{log}\n{text}"
    );
    let put = log
        .lines()
        .find(|l| l.starts_with("PUT "))
        .unwrap_or_else(|| panic!("the run step is completed\n{log}\n{text}"));
    let main_ts = git(
        &yard.repo,
        0,
        &["log", "-1", "--format=%ct", "refs/remotes/origin/main"],
    );
    for field in [
        "\"result\":\"ok\"".to_string(),
        "\"worktree_pass\":\"ran\"".to_string(),
        "\"worktrees_removed\":\"3\"".to_string(),
        "\"worktrees_kept_live\":\"1\"".to_string(),
        "\"worktrees_kept_dirty\":\"1\"".to_string(),
        "\"targets_removed\":\"1\"".to_string(),
        // The ref the judgement was made against, and its commit time,
        // so a pass on a stale fetch is visible on the packet.
        format!("\"origin_main_sha\":\"{main}\""),
        format!("\"origin_main_ref_ts\":\"{main_ts}\""),
    ] {
        assert!(
            put.contains(&field),
            "the run step carries {field}\n{put}\n{text}"
        );
    }
    assert!(
        put.contains("agent-dirty"),
        "the kept dirty tree is named on the packet too\n{put}\n{text}"
    );
}

/// A checkout with no origin/main cannot say what landed. The pass
/// removes nothing, and — unlike the H11 pass, which returned in
/// silence — RECORDS the skip and its reason on its packet: a skipped
/// pass is a finding, not silence (backlog b50a65ef).
#[test]
fn a_checkout_without_origin_main_skips_the_pass_and_records_why() {
    let root = boss_testing::scratch_dir("boss-dsr-noref");
    let _guard = Scratch(root.clone());
    let yard = Yard::new(&root);
    let gone = yard.worktree("agent-gone", Some("feat/gone"), 200);
    git(
        &yard.repo,
        0,
        &["update-ref", "-d", "refs/remotes/origin/main"],
    );

    let out = run(&root, &[]);
    let text = say(&out);
    assert!(
        gone.exists(),
        "a pass that cannot answer removes nothing\n{text}"
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("worktree pass skipped"),
        "the skip is said out loud, on stderr\n{text}"
    );
    assert!(
        ls_remote_calls(&root).is_empty(),
        "and never by asking the forge\n{}\n{text}",
        ls_remote_calls(&root)
    );
    let log = curl_log(&root);
    assert!(
        log.contains("POST http://sor.test:7900/api/jobs "),
        "a pass that could not answer files its packet\n{log}\n{text}"
    );
    let put = log
        .lines()
        .find(|l| l.starts_with("PUT "))
        .unwrap_or_else(|| panic!("the run step is completed\n{log}\n{text}"));
    assert!(
        put.contains("\"worktree_pass\":\"skipped\"")
            && put.contains("\"worktree_pass_reason\":\"")
            && put.contains("origin/main")
            && put.contains("\"result\":\"incomplete\""),
        "the packet carries the skip AND why, as an incomplete pass\n{put}\n{text}"
    );
}

/// The mtime of a worktree's reflog, as epoch seconds.
fn reflog_mtime(worktree: &Path) -> u64 {
    let gitdir = PathBuf::from(git(worktree, 0, &["rev-parse", "--absolute-git-dir"]));
    std::fs::metadata(gitdir.join("logs/HEAD"))
        .and_then(|m| m.modified())
        .expect("reflog mtime")
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_secs()
}

/// ONE repo-wide git operation must not read as activity in every
/// worktree at once. Measured 2026-09-21 (backlog adce5171): the gone-
/// branch pass removed nothing and kept 373 of 397 worktrees "with git
/// activity inside the window", because the idle clock took the MAX of
/// four signals and one of them — the mtime of `$gitdir/logs/HEAD` —
/// read EXACTLY 32h on three unrelated worktrees whose own HEAD, index
/// and directory were 73h, 80h and 101h quiet. Re-measured 2026-09-23:
/// 154 worktrees' reflogs rewritten inside four seconds at 2026-09-22
/// 05:43:30Z, beside a write of info/refs and objects/info — a `git gc`,
/// whose `reflog expire --all` rewrites every worktree's reflog whether
/// or not an entry expires. The fixture does exactly that, for real,
/// and the three trees idle past their windows must still go.
///
/// What a reflog SAYS is kept as a signal: each entry carries the time
/// git wrote it, and an expire copies entries without redating them.
/// So a tree whose only recent act is a reflog entry (a reset that
/// moved no file this pass reads) is kept.
#[test]
fn a_repo_wide_reflog_rewrite_is_not_activity_in_every_worktree() {
    let root = boss_testing::scratch_dir("boss-dsr-gc-touch");
    let _guard = Scratch(root.clone());
    let yard = Yard::new(&root);

    let landed = yard.worktree("agent-landed", Some("feat/landed"), 30);
    yard.land("feat/landed");
    let abandoned = yard.worktree("agent-abandoned", Some("feat/abandoned"), 60);
    let detached = yard.worktree("agent-detached", None, 24 * 10);
    // A detached head the pass may remove is one some ref still holds —
    // here a forge PR ref, the shape `refs/pulls/<n>` takes on the pod.
    let detached_head = git(&detached, 0, &["rev-parse", "HEAD"]);
    git(
        &yard.repo,
        0,
        &["update-ref", "refs/pulls/1", &detached_head],
    );
    // Quiet by every file signal for 60h, but its reflog's newest ENTRY
    // is an hour old: someone is working here.
    let working = yard.worktree("agent-working", Some("feat/working"), 60);
    git(&working, 1, &["reset", "-q", "--soft", "HEAD"]);
    let gitdir = PathBuf::from(git(&working, 0, &["rev-parse", "--absolute-git-dir"]));
    for p in [gitdir.join("HEAD"), gitdir.join("index"), working.clone()] {
        touch_at(&p, 60);
    }

    // The repo-wide operation: what `git gc` runs first.
    git(&yard.repo, 0, &["reflog", "expire", "--all"]);
    let now = unix_now();
    for wt in [&landed, &abandoned, &detached, &working] {
        assert!(
            now.saturating_sub(reflog_mtime(wt)) < 600,
            "precondition: the expire rewrote {}'s reflog, as a gc does",
            wt.display()
        );
    }

    let out = run(&root, &[]);
    let text = say(&out);
    assert!(
        !landed.exists(),
        "landed, idle 30h against a 12h grace: a gc's reflog rewrite is not activity\n{text}"
    );
    assert!(
        !abandoned.exists(),
        "unpushed, idle 60h against 48h: removed despite the gc\n{text}"
    );
    assert!(
        !detached.exists(),
        "detached, idle 10 days, its head on a ref: removed despite the gc\n{text}"
    );
    assert!(
        working.exists(),
        "a reflog ENTRY an hour old is activity, whatever the other files say\n{text}"
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("worktree pass: removed 3"),
        "the totals say so\n{text}"
    );
}

/// A detached HEAD whose commit no ref holds is UNPUSHED WORK: the
/// worktree's own HEAD and reflog are the only things naming it, and
/// `git worktree remove` deletes both, leaving the commit to the next
/// gc. Measured 2026-09-23 on the pod: of the 16 detached worktrees the
/// corrected idle clock makes due, 13 sit on a ref (a forge PR ref, a
/// branch) and 3 — preflight-2026-09-10b, preflight-3way, preflight-f73
/// — on none. Those are kept and NAMED, on the log and on the packet,
/// the way a dirty tree is, so an operator can decide.
#[test]
fn a_detached_worktree_whose_head_no_ref_holds_is_kept_and_named() {
    let root = boss_testing::scratch_dir("boss-dsr-orphan-head");
    let _guard = Scratch(root.clone());
    let yard = Yard::new(&root);
    let orphan = yard.worktree("agent-orphan", None, 24 * 10);
    // One tree the pass does remove, so it acts and files its packet.
    let landed = yard.worktree("agent-landed", Some("feat/landed"), 30);
    yard.land("feat/landed");

    let out = run(&root, &[]);
    let text = say(&out);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        orphan.exists(),
        "a commit only this worktree names is never thrown to the gc\n{text}"
    );
    assert!(
        stdout.contains("agent-orphan") && stdout.contains("no ref holds"),
        "the kept tree is named with why\n{text}"
    );
    assert!(
        stdout.contains("1 with a head no ref holds"),
        "and counted on the totals line\n{text}"
    );
    assert!(!landed.exists(), "the landed tree beside it goes\n{text}");
    let log = curl_log(&root);
    let put = log
        .lines()
        .find(|l| l.starts_with("PUT "))
        .unwrap_or_else(|| panic!("the run step is completed\n{log}\n{text}"));
    assert!(
        put.contains("\"worktrees_kept_unreferenced\":\"1\"")
            && put.contains("\"worktrees_kept_unreferenced_names\":\"agent-orphan"),
        "the packet names the kept tree too\n{put}\n{text}"
    );
}

// ---------------------------------------------------------------------
// THE CLI LEG (backlog c35eda6c, retro 27fad542, 2026-09-18): the pod's
// `boss` is the tree's by construction. Each pass takes the CLI at
// origin/main out of the cluster image with the estate's installer
// into a store under /work the shim (infra/dev/boss) reads. The retro
// counted the CLI rebuilt by hand three times in one window because a
// landed car changed what it validates locally; this pass is what
// deletes the rebuild. The installer is stubbed here — its own pin
// (install_cli_from_image_sh.rs) drives the registry protocol — and
// what is pinned is the HANDOFF: the sha, the store, the link, and the
// registry host rendered from infra/estate/estate.toml, never a literal.
// ---------------------------------------------------------------------

/// A stub installer on disk: records its argument and the environment
/// the pass handed it, and exits with `STUB_INSTALLER_RC`.
fn stub_installer(root: &Path) -> PathBuf {
    let path = root.join("bin").join("install-cli");
    boss_testing::create_dir(&root.join("bin"));
    boss_testing::write_exec(
        &path,
        concat!(
            "#!/usr/bin/env bash\n",
            "printf 'sha=%s store=%s link=%s sor_env=%s\\n' \"$1\" \"${BOSS_CLI_STORE:-}\" \"${BOSS_CLI_LINK:-}\" \"${BOSS_SOR_ENV:-}\" >> \"$STUB_INSTALL_LOG\"\n",
            "case \"${STUB_INSTALLER_RC:-0}\" in\n",
            "    0) echo \"install-cli-from-image: CONFIRMED — stub at $1\" ;;\n",
            "    75) echo \"install-cli-from-image: NOT YET — no image for $1 (stub)\" >&2 ;;\n",
            "    *) echo \"install-cli-from-image: REFUSED — stub refusal for $1\" >&2 ;;\n",
            "esac\n",
            "exit \"${STUB_INSTALLER_RC:-0}\"\n",
        ),
    );
    path
}

fn install_log(root: &Path) -> String {
    std::fs::read_to_string(root.join("install-log.txt")).unwrap_or_default()
}

/// What infra/estate/estate.toml spells as the registry — read, not typed.
fn tree_registry() -> String {
    let toml = std::fs::read_to_string(repo_root().join("infra/estate/estate.toml"))
        .expect("infra/estate/estate.toml");
    toml.lines()
        .find_map(|l| l.strip_prefix("forge_registry = \""))
        .and_then(|v| v.strip_suffix('"'))
        .expect("estate.toml spells forge_registry")
        .to_string()
}

#[test]
fn each_pass_installs_the_trees_cli_from_the_image_through_the_estate_installer() {
    let root = boss_testing::scratch_dir("boss-dsr-cli");
    let _guard = Scratch(root.clone());
    let yard = Yard::new(&root);
    let main = git(&yard.repo, 0, &["rev-parse", "refs/remotes/origin/main"]);
    let store = root.join("work").join("tools").join("image-cli");
    let link = root
        .join("work")
        .join("tools")
        .join("bin")
        .join("boss-image");

    // Confirmed: the sha is the checkout's origin/main — the ref the
    // shim compares against, and the only one the sidecar can read (it
    // has no forge credential) — the store and link are under the work
    // mount, and the registry reaches the installer as a rendered
    // sor.env (BOSS_SOR_ENV) whose host is estate.toml's.
    let out = run(&root, &[]);
    let text = say(&out);
    assert!(out.status.success(), "{text}");
    let log = install_log(&root);
    let want = format!(
        "sha={main} store={} link={} sor_env={}",
        store.display(),
        link.display(),
        store.join("sor.env").display()
    );
    assert_eq!(log.trim(), want, "the handoff to the installer\n{text}");
    let rendered = std::fs::read_to_string(store.join("sor.env")).expect("the rendered sor.env");
    assert!(
        rendered.contains(&format!("BOSS_FORGE_REGISTRY_HOST={}\n", tree_registry())),
        "the registry host is rendered from estate.toml, never a literal:\n{rendered}\n{text}"
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("cli: install-cli-from-image: CONFIRMED") && stdout.contains(&main[..8]),
        "the installer's output rides the log, prefixed, and the sha is named\n{text}"
    );
    assert!(
        !curl_log(&root).contains("POST"),
        "a confirmed install alone files no packet\n{}\n{text}",
        curl_log(&root)
    );

    // Not yet: the image is not built — a wait, logged, not a problem.
    let out = run(&root, &[("STUB_INSTALLER_RC", "75")]);
    let text = say(&out);
    assert!(out.status.success(), "not yet is not a failure\n{text}");
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("not yet") && text.contains(&main[..8]),
        "the wait is said, with the sha\n{text}"
    );
    assert!(
        !curl_log(&root).contains("POST"),
        "and files nothing\n{text}"
    );

    // Refused: a real fault (a dark registry, a digest mismatch) is a
    // problem — loud, exit 1, and on a packet with the exit named.
    let out = run(&root, &[("STUB_INSTALLER_RC", "1")]);
    let text = say(&out);
    assert_eq!(
        out.status.code(),
        Some(1),
        "a refusal reds the pass\n{text}"
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("CLI install FAILED"),
        "the failure is loud, on stderr\n{text}"
    );
    let log = curl_log(&root);
    let put = log
        .lines()
        .find(|l| l.starts_with("PUT "))
        .unwrap_or_else(|| panic!("the run step is completed\n{log}\n{text}"));
    assert!(
        put.contains(&format!("\"cli_sha\":\"{main}\""))
            && put.contains("\"cli_result\":\"failed: exit 1\""),
        "the packet names the sha and the verdict\n{put}\n{text}"
    );

    // `--cli` runs the CLI leg alone — what the shim's refusal names as
    // the ten-second fix — and none of the reclaim passes.
    std::fs::remove_file(root.join("install-log.txt")).expect("reset the install log");
    let out = Command::new("bash")
        .arg(repo_root().join("infra/cluster/dev-scratch-reclaim.sh"))
        .arg("--cli")
        .env("BOSS_CLI_INSTALLER", stub_installer(&root))
        .env("STUB_INSTALL_LOG", root.join("install-log.txt"))
        .env("SCRATCH_MOUNT", &root)
        .env("WORK_MOUNT", root.join("work"))
        .env("REPO_DIR", &yard.repo)
        .env("BOSS_JOBS_URL", "http://sor.test:7900")
        .output()
        .expect("run --cli");
    let text = say(&out);
    assert!(out.status.success(), "{text}");
    assert!(
        install_log(&root).contains(&format!("sha={main} ")),
        "{text}"
    );
    assert!(
        !String::from_utf8_lossy(&out.stdout).contains("worktree pass"),
        "--cli runs no reclaim pass\n{text}"
    );

    // `--cli` is a STATUS the shim reads, not prose (backlog 49d9e99d,
    // 2026-09-18): the shim runs this leg itself before a write and
    // must tell "installed, proceed" from "not built yet, wait" without
    // parsing the log. Not yet exits 75 — the installer's own code —
    // while the hourly pass above still counts it as a wait, exit 0.
    let out = Command::new("bash")
        .arg(repo_root().join("infra/cluster/dev-scratch-reclaim.sh"))
        .arg("--cli")
        .env("BOSS_CLI_INSTALLER", stub_installer(&root))
        .env("STUB_INSTALLER_RC", "75")
        .env("STUB_INSTALL_LOG", root.join("install-log.txt"))
        .env("SCRATCH_MOUNT", &root)
        .env("WORK_MOUNT", root.join("work"))
        .env("REPO_DIR", &yard.repo)
        .env("BOSS_JOBS_URL", "http://sor.test:7900")
        .output()
        .expect("run --cli");
    let text = say(&out);
    assert_eq!(
        out.status.code(),
        Some(75),
        "--cli answers not yet with the installer's 75, so the shim can wait on it\n{text}"
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("not yet"),
        "and still says so\n{text}"
    );
}

/// A checkout with no origin/main — never fetched — names no sha to
/// install, and the pass says so rather than guess one.
#[test]
fn a_checkout_without_origin_main_installs_no_cli() {
    let root = boss_testing::scratch_dir("boss-dsr-cli-noref");
    let _guard = Scratch(root.clone());
    let yard = Yard::new(&root);
    git(
        &yard.repo,
        0,
        &["update-ref", "-d", "refs/remotes/origin/main"],
    );
    let out = run(&root, &[]);
    let text = say(&out);
    assert!(
        install_log(&root).is_empty(),
        "no origin/main, no install\n{text}"
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("CLI install skipped"),
        "the skip is said out loud, on stderr\n{text}"
    );
}

// ---------------------------------------------------------------------
// THE FAST-FORWARD PASS (backlog 033d1fd3, 2026-09-19).
//
// Car fix/a-door-refuses-to-answer-from-a-stale-checkout made every pod
// door judge its own copy: a read WARNS and a boss-api write is REFUSED
// when /work/boss is behind origin/main. That is the correctness half.
// The ergonomics half was still a human act — somebody typing `git -C
// /work/boss merge --ff-only origin/main` — and the operator session
// did exactly that FOUR TIMES on 2026-09-19 alone (session start, 17:05
// after the 404, 17:40, 18:00) while trains land roughly every 50
// minutes. A warning everybody expects is a warning nobody reads, which
// is how the class comes back; mechanical operations belong to the
// machine (CLAUDE.md §Diagnosis).
//
// The pass takes NO network: it fast-forwards to the
// refs/remotes/origin/main the checkout already has, which is the same
// ref door-freshness.sh compares against — so the door can only call a
// copy stale in the window where this pass can repair it, and the
// sidecar's missing forge credential (b50a65ef) never comes into it.
//
// THE HAZARD IT MUST NOT CAUSE: `boss gate` renders its runner from the
// tree AT LAUNCH, and mutating the tree under a LAUNCHING gate is the
// never-stash-while-a-gate-runs fault. The quiet is read from the
// SYSTEM OF RECORD — a recently-opened `gate-run` packet — rather than
// from a lock file, because the gate-runs are already in the record. A
// reading it cannot take is a DEFER, never a fast-forward taken blind.
//
// AND LAUNCHING IS NOT RUNNING (backlog 475fbd10, 2026-09-22). `boss
// gate` takes everything it will ever take from a tree in ONE
// `read_to_string` of the runner manifest — the first statement of
// `gate::run`, before the gate-run packet is filed — so a packet older
// than the launch window belongs to a gate that has already rendered.
// Deferring on "any open gate-run" was true for 251 of the last 300
// minutes (84%, measured on the packet; 73% re-measured here from the
// gate-run history), against 15% for a 120-second launch window, and an
// hourly pass against an 84%-busy condition lands about one time in
// six. The checkout sat five commits behind and every door warned.
//
// AND THE DEFERRAL HAS AN UPPER BOUND, because one that can repeat
// forever never errors — it just stops being true, which is the silent
// -failure class CLAUDE.md names. Past the deadline a deferral stops
// being a wait and becomes a finding: a problem, a red pass, and the
// reason on the packet where the refused fast-forward already lands.
// ---------------------------------------------------------------------

/// The checkout's own HEAD — what the doors run from.
fn head_sha(repo: &Path) -> String {
    git(repo, 0, &["rev-parse", "HEAD"])
}

/// A fixture whose checkout is one train behind origin/main, the shape
/// the pod is in a few minutes after every merge. The worktree that
/// carried the commit is one hour old, so the worktree pass keeps it and
/// the fast-forward is the only thing the run can act on.
fn behind_by_one(root: &Path) -> Yard {
    let yard = Yard::new(root);
    yard.worktree("agent-ahead", Some("feat/ahead"), 1);
    yard.land("feat/ahead");
    yard
}

#[test]
fn a_checkout_behind_origin_main_is_fast_forwarded_when_no_gate_is_reading_the_tree() {
    let root = boss_testing::scratch_dir("boss-dsr-ff");
    let _guard = Scratch(root.clone());
    let yard = behind_by_one(&root);
    let main = yard.origin_main();
    assert_ne!(head_sha(&yard.repo), main, "the fixture starts behind");

    let out = run(&root, &[]);
    let text = say(&out);
    assert_eq!(
        head_sha(&yard.repo),
        main,
        "the checkout the doors run from is taken to origin/main\n{text}"
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("fast-forward") && stdout.contains(&main[..8]),
        "the pass says what it moved and to which sha\n{text}"
    );
    let log = curl_log(&root);
    assert!(
        log.contains("kind=gate-run") && log.contains("status=open"),
        "the quiet is read from the system of record, not from a lock file\n{log}\n{text}"
    );
    // A routine fast-forward is maintenance that worked, like the CLI
    // install beside it: it rides the log, not a packet (David
    // 2026-09-16 — telemetry lives outside the audit log).
    assert!(
        !log.contains("POST"),
        "a fast-forward that worked files no packet\n{log}\n{text}"
    );
    // And the checkout already standing on origin/main is a no-op that
    // asks the system of record nothing at all.
    std::fs::remove_file(root.join("curl-log.txt")).expect("reset the curl log");
    let out = run(&root, &[]);
    assert!(
        !curl_log(&root).contains("kind=gate-run"),
        "a checkout that is already current asks nothing\n{}\n{}",
        curl_log(&root),
        say(&out)
    );
}

#[test]
fn a_gate_run_opened_this_second_defers_the_fast_forward() {
    let root = boss_testing::scratch_dir("boss-dsr-ff-gate");
    let _guard = Scratch(root.clone());
    let yard = behind_by_one(&root);
    let before = head_sha(&yard.repo);

    let out = run(&root, &[("STUB_OPEN_GATE", "1")]);
    let text = say(&out);
    assert_eq!(
        head_sha(&yard.repo),
        before,
        "a gate is reading a tree — the pass must not move one\n{text}"
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("gate-run"),
        "and it says which reading held it back\n{text}"
    );
    assert!(
        out.status.success(),
        "waiting for the next hour is not a fault\n{text}"
    );
}

/// A gate that opened its packet an hour ago read the runner manifest
/// an hour ago too — `gate::run`'s one `read_to_string` runs BEFORE the
/// packet is filed. Holding the checkout for it buys nothing and costs
/// the 84% of the day at least one gate is open (backlog 475fbd10).
#[test]
fn a_gate_that_has_already_rendered_its_runner_does_not_defer_the_fast_forward() {
    let root = boss_testing::scratch_dir("boss-dsr-ff-running");
    let _guard = Scratch(root.clone());
    let yard = behind_by_one(&root);
    let main = yard.origin_main();

    let out = run(
        &root,
        &[("STUB_OPEN_GATE", "1"), ("STUB_GATE_AGE_SECS", "3600")],
    );
    let text = say(&out);
    assert_eq!(
        head_sha(&yard.repo),
        main,
        "an hour-old gate has taken everything it will ever take from a tree\n{text}"
    );
    assert!(
        out.status.success(),
        "and moving the checkout for it is the pass working\n{text}"
    );
}

/// A page that reports more open gate-runs than it returns answers a
/// smaller question (CLAUDE.md — a limit is not a filter): the launch
/// window cannot be judged from rows that were never sent, so the pass
/// defers the way it does on any unread check.
#[test]
fn a_truncated_gate_run_page_is_a_reading_the_pass_cannot_take() {
    let root = boss_testing::scratch_dir("boss-dsr-ff-truncated");
    let _guard = Scratch(root.clone());
    let yard = behind_by_one(&root);
    let before = head_sha(&yard.repo);

    // One row returned, three said to be open, and the two unseen ones
    // could each have launched a second ago.
    let out = run(
        &root,
        &[
            ("STUB_OPEN_GATE", "1"),
            ("STUB_GATE_AGE_SECS", "3600"),
            ("STUB_GATE_TOTAL", "3"),
        ],
    );
    let text = say(&out);
    assert_eq!(
        head_sha(&yard.repo),
        before,
        "a fast-forward is never taken on a page that answered a smaller question\n{text}"
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("fast-forward deferred"),
        "the defer and its reason are loud\n{text}"
    );
}

/// THE UPPER BOUND. A deferral that can repeat forever never errors; it
/// just stops being true, and the checkout starves while every door
/// warns and every write is refused at exit 78. How long it has been
/// behind is read from GIT ALONE — the committer time of the oldest
/// commit the checkout is missing — so there is no counter file and no
/// second copy of a fact (CLAUDE.md §9a).
#[test]
fn a_deferral_that_outlives_its_deadline_is_a_problem_on_the_packet() {
    let root = boss_testing::scratch_dir("boss-dsr-ff-deadline");
    let _guard = Scratch(root.clone());
    let yard = behind_by_one(&root);
    let before = head_sha(&yard.repo);

    let out = run(
        &root,
        &[("STUB_OPEN_GATE", "1"), ("BOSS_FF_DEADLINE_SECS", "0")],
    );
    let text = say(&out);
    assert_eq!(
        head_sha(&yard.repo),
        before,
        "the deadline makes a stuck deferral LOUD; it never overrides the hazard\n{text}"
    );
    assert_eq!(
        out.status.code(),
        Some(1),
        "and reds the pass, like every other problem here\n{text}"
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("DEFERRED PAST ITS DEADLINE"),
        "a deferral nobody can see is the failure this bound exists to end\n{text}"
    );
    let log = curl_log(&root);
    let put = log
        .lines()
        .find(|l| l.starts_with("PUT "))
        .unwrap_or_else(|| panic!("the run step is completed\n{log}\n{text}"));
    assert!(
        put.contains("\"result\":\"incomplete\"") && put.contains("past its"),
        "the packet says the checkout stopped catching up\n{put}\n{text}"
    );
    assert!(
        put.contains("\"ff_detail\":\"") && put.contains("behind origin/main for"),
        "and names how long and what held it — not an exit code to go re-derive\n{put}\n{text}"
    );
}

#[test]
fn a_system_of_record_that_cannot_be_read_defers_the_fast_forward() {
    let root = boss_testing::scratch_dir("boss-dsr-ff-dark");
    let _guard = Scratch(root.clone());
    let yard = behind_by_one(&root);
    let before = head_sha(&yard.repo);

    // curl exit 22: the API ANSWERED an error. A safety check that did
    // not run is not a safety check — the tree stays where it is.
    let out = run(&root, &[("STUB_GATE_RC", "22")]);
    let text = say(&out);
    assert_eq!(
        head_sha(&yard.repo),
        before,
        "a fast-forward is never taken on an unread gate check\n{text}"
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("fast-forward deferred"),
        "the defer and its reason are loud\n{text}"
    );
}

#[test]
fn a_checkout_that_is_not_plainly_behind_on_main_is_left_alone() {
    let root = boss_testing::scratch_dir("boss-dsr-ff-diverged");
    let _guard = Scratch(root.clone());
    let yard = behind_by_one(&root);
    // A commit of its own on main: this checkout has diverged, which is
    // a developer's doing and not a checkout that forgot to pull — the
    // same line door-freshness.sh draws when it declines to judge.
    boss_testing::write_file(&yard.repo.join("local.txt"), "mine\n");
    git(&yard.repo, 0, &["add", "."]);
    git(&yard.repo, 0, &["commit", "-q", "-m", "local"]);
    let before = head_sha(&yard.repo);

    let out = run(&root, &[]);
    let text = say(&out);
    assert_eq!(
        head_sha(&yard.repo),
        before,
        "a diverged checkout is never rewound or merged\n{text}"
    );
    assert!(
        curl_log(&root).is_empty(),
        "and the system of record is not asked about a tree that cannot move\n{}\n{text}",
        curl_log(&root)
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("fast-forward skipped")
            && String::from_utf8_lossy(&out.stdout).contains("diverged"),
        "the pass names what it found instead\n{text}"
    );
}

/// git's own refusal is the last lock, and it is a FINDING: the doors
/// keep warning until somebody looks, so the pass says so on stderr,
/// reds the run, and puts the verdict on its packet — never a silent
/// checkout that quietly stopped catching up.
#[test]
fn a_fast_forward_git_refuses_is_loud_and_lands_on_the_packet() {
    let root = boss_testing::scratch_dir("boss-dsr-ff-refused");
    let _guard = Scratch(root.clone());
    let yard = behind_by_one(&root);
    let before = head_sha(&yard.repo);
    // A local file the incoming commit also adds: git refuses rather
    // than overwrite it. (Measured 2026-09-20: /work/boss carries a
    // modified, tracked .claude/settings.json, so this is the live
    // shape — and the reason the pass leans on git's narrow refusal
    // instead of demanding a pristine tree, which would never run.)
    boss_testing::write_file(&yard.repo.join("work.txt"), "mine, uncommitted\n");

    let out = run(&root, &[]);
    let text = say(&out);
    assert_eq!(
        head_sha(&yard.repo),
        before,
        "a refused fast-forward moves nothing\n{text}"
    );
    assert_eq!(
        out.status.code(),
        Some(1),
        "and reds the pass, like every other problem here\n{text}"
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("fast-forward FAILED")
            && String::from_utf8_lossy(&out.stderr).contains("work.txt"),
        "git's complete complaint rides the log — a digest would throw away the only copy\n{text}"
    );
    let log = curl_log(&root);
    let put = log
        .lines()
        .find(|l| l.starts_with("PUT "))
        .unwrap_or_else(|| panic!("the run step is completed\n{log}\n{text}"));
    assert!(
        put.contains("\"ff_result\":\"failed: exit")
            && put.contains(&format!("\"ff_to\":\"{}\"", yard.origin_main()))
            && put.contains("\"result\":\"incomplete\""),
        "the packet names the verdict and the sha it could not reach\n{put}\n{text}"
    );
    // AND WHAT GIT ACTUALLY SAID (backlog 6db0b658; David, 2026-09-22 —
    // accept the jam, make the refusal loud). `failed: exit 1` is a
    // verdict a reader must go and re-derive from a journal they are not
    // reading: CLAUDE.md §Diagnosis, "a verdict must name what failed".
    // The offending path is the one fact that turns the packet into an
    // action, and it was in the log all along.
    assert!(
        put.contains("\"ff_detail\":\""),
        "the packet carries git's own complaint, not only an exit code\n{put}\n{text}"
    );
    assert!(
        put.contains("work.txt"),
        "and it NAMES THE FILE — without that the reader has an exit code and a sha, and \
         must go to the host's journal to learn which path refused\n{put}\n{text}"
    );
}
