//! `infra/gcp/reclaim-gcp-root.sh` is RUN, not read — against a scratch
//! `/opt`, a scratch process table, mount table and unit directory, a
//! stubbed `systemctl` and a stubbed `journalctl`, so every verdict
//! below is one the script actually reached and every removal is one it
//! actually made.
//!
//! WHY THE VERB EXISTS (backlog d3c7eada car 2, 2026-09-26). boss-gcp's
//! 48 GB root sat at 11-13 GB free for nine days against its 17 GB
//! floor. Car 1's disk-report reading found ~2.3 GB of hand-made July
//! 2026 binary backups under /opt (`boss-binbak-*`, `boss-dev-bak`) and
//! a 4.1 GB journal, beside DATA that is David's call — the cluster-pg
//! dumps and the second-stack capture under /var/backups, the home
//! directories, /usr/local, and the live /opt/boss and /opt/boss-cli. So
//! the reclaim is a MUTATING approval verb whose paths are the script's
//! own: `--dry-run` renders a plan, David's passkey signs its hash, and
//! the write removes only what that signed plan names.
//!
//! What each case pins: the dry run renders a deterministic plan that
//! names its own hash and removes nothing; the signed hash removes
//! exactly the backups and leaves every neighbour intact; a plan that
//! moved, and a second run of an applied one, remove nothing; a path
//! handed to the script, a symlinked candidate, a checkout, a backup
//! COPIED today with its old mtimes (the adversarial review's H1), and a
//! backup that a live tree, a mount, a symlink, a running process, a
//! loaded unit or a unit file on disk resolves into are each refused
//! with nothing removed; a unit bound that cannot be evaluated is a
//! refusal; and, through `ops-runner.sh` with the shipped verb files,
//! the plan verb is answered on boss-gcp while a path, or a hash with no
//! approval, never reaches the write.

use boss_testing::{repo_root, scratch_dir, write_exec, write_file};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn has(tool: &str) -> bool {
    Command::new("sh")
        .args(["-c", &format!("command -v {tool} >/dev/null 2>&1")])
        .status()
        .is_ok_and(|s| s.success())
}

const SCRIPT: &str = "infra/gcp/reclaim-gcp-root.sh";

/// The four backups car 1 read on boss-gcp, by their real names.
const BACKUPS: [&str; 4] = [
    "boss-binbak-pre-pr73-0702-1801",
    "boss-binbak-pre-latest-194355",
    "boss-binbak-pre-pr5-20260701-032322",
    "boss-dev-bak",
];

/// Neighbours that must survive every run: the live trees, and names
/// that only resemble a backup.
const KEPT: [&str; 4] = ["boss", "boss-cli", "boss-dev-bak2", "boss-binbak"];

fn epoch_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

/// The clock every case runs at unless it says otherwise: sixty days on,
/// so a fixture written a moment ago is old by ctime — which a test
/// cannot set any other way.
fn sixty_days_on() -> String {
    (epoch_now() + 60 * 86400).to_string()
}

/// `write_file`, creating the parents first.
fn put(path: &Path, body: &str) {
    std::fs::create_dir_all(path.parent().expect("a parent")).unwrap();
    write_file(path, body);
}

/// Age every file and directory under `p` by 60 days BY MTIME, the way
/// the July backups are on the host — and the way `cp -a` carries them.
fn age(p: &Path) {
    let ok = Command::new("find")
        .arg(p)
        .args(["-exec", "touch", "-h", "-d", "60 days ago", "{}", "+"])
        .status()
        .expect("find runs")
        .success();
    assert!(ok, "could not age {}", p.display());
}

fn sha256(bytes: &str) -> String {
    let dir = scratch_dir("reclaim-gcp-root-sha");
    let f = dir.join("bytes");
    write_file(&f, bytes);
    let out = Command::new("sha256sum")
        .arg(&f)
        .output()
        .expect("sha256sum");
    String::from_utf8_lossy(&out.stdout)[..64].to_string()
}

struct Case {
    root: PathBuf,
    bin: PathBuf,
    opt: PathBuf,
    proc_dir: PathBuf,
    links: PathBuf,
    units: PathBuf,
    mountinfo: PathBuf,
    calls: PathBuf,
}

struct Run {
    code: i32,
    out: String,
    err: String,
}

impl Run {
    fn text(&self) -> String {
        format!("{}{}", self.out, self.err)
    }

    /// The hash the dry run named on stderr.
    fn plan_sha(&self) -> String {
        self.err
            .lines()
            .find_map(|l| l.strip_prefix("plan-sha256: "))
            .unwrap_or_else(|| panic!("no plan-sha256 line:\n{}", self.text()))
            .to_string()
    }
}

impl Case {
    fn new(name: &str) -> Self {
        let root = scratch_dir(&format!("reclaim-gcp-root-{name}"));
        let bin = root.join("bin");
        let opt = root.join("opt");
        let proc_dir = root.join("proc");
        let links = root.join("links");
        let units = root.join("units");
        for d in [&bin, &opt, &proc_dir, &links, &units] {
            std::fs::create_dir_all(d).unwrap();
        }
        for b in BACKUPS.iter().chain(KEPT.iter()) {
            put(&opt.join(b).join("bin/boss-jobs-api"), "binary\n");
            put(&opt.join(b).join("VERSION"), "july\n");
        }
        age(&opt);
        // One running process, whose binary is the live tree's.
        std::fs::create_dir_all(proc_dir.join("101")).unwrap();
        std::os::unix::fs::symlink(opt.join("boss/bin/boss-jobs-api"), proc_dir.join("101/exe"))
            .unwrap();
        write_file(&proc_dir.join("101/comm"), "boss-jobs-api\n");
        // A unit file on disk that runs from the live tree.
        write_file(
            &units.join("boss-jobs-api.service"),
            &format!(
                "[Service]\nExecStart={}\n",
                opt.join("boss/bin/boss-jobs-api").display()
            ),
        );
        // The mount table: root, and one mount elsewhere.
        let mountinfo = root.join("mountinfo");
        write_file(
            &mountinfo,
            "22 1 8:1 / / rw,relatime shared:1 - ext4 /dev/sda1 rw\n\
             40 22 8:17 / /var/lib/boss-build rw,relatime shared:2 - ext4 /dev/sdc rw\n",
        );
        let calls = root.join("calls.log");
        write_exec(
            &bin.join("systemctl"),
            r#"#!/bin/sh
# stub systemctl: two loaded services; show prints STUB_EXEC for the
# first and a /usr path for the second.
echo "systemctl $*" >> "$STUB_CALLS"
case "$1" in
  list-units)
    [ -n "${STUB_LIST_FAIL:-}" ] && { echo "Failed to connect to bus (stub)" >&2; exit 1; }
    echo "boss-ops-runner.service loaded active running ops"
    echo "boss-ml-api.service loaded active running ml"
    ;;
  show)
    [ -n "${STUB_SHOW_FAIL:-}" ] && { echo "Failed to get properties (stub)" >&2; exit 1; }
    for a in "$@"; do u="$a"; done
    case "$u" in
      boss-ops-runner.service) echo "{ path=${STUB_EXEC:-/usr/bin/true} ; argv[]=${STUB_EXEC:-/usr/bin/true} * ; }" ;;
      *) echo "{ path=/usr/bin/env ; argv[]=/usr/bin/env ; }"; echo "/etc/systemd/system/$u" ;;
    esac
    ;;
  *) echo "stub systemctl: unexpected $*" >&2; exit 99 ;;
esac
"#,
        );
        write_exec(
            &bin.join("journalctl"),
            "#!/bin/sh\n# stub journalctl: records its argv\necho \"journalctl $*\" >> \"$STUB_CALLS\"\n\
             case \"$1\" in --disk-usage) echo 'Archived and active journals take up 4.1G in the file system.';; esac\n",
        );
        Self {
            root,
            bin,
            opt,
            proc_dir,
            links,
            units,
            mountinfo,
            calls,
        }
    }

    fn env(&self, cmd: &mut Command) {
        cmd.env_clear()
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    self.bin.display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env("STUB_CALLS", &self.calls)
            .env("BOSS_RECLAIM_OPT_DIR", &self.opt)
            .env("BOSS_RECLAIM_PROC_DIR", &self.proc_dir)
            .env("BOSS_RECLAIM_MOUNTINFO", &self.mountinfo)
            .env("BOSS_RECLAIM_UNIT_DIRS", &self.units)
            .env("BOSS_RECLAIM_NOW", sixty_days_on())
            .env(
                "BOSS_RECLAIM_LINK_DIRS",
                format!("{} {}", self.opt.display(), self.links.display()),
            );
    }

    fn run(&self, args: &[&str]) -> Run {
        self.run_env(args, &[])
    }

    fn run_env(&self, args: &[&str], extra: &[(&str, String)]) -> Run {
        let mut cmd = Command::new("bash");
        cmd.arg(repo_root().join(SCRIPT)).args(args);
        self.env(&mut cmd);
        for (k, v) in extra {
            cmd.env(k, v);
        }
        let out = cmd.output().expect("reclaim-gcp-root.sh runs");
        Run {
            code: out.status.code().unwrap_or(-1),
            out: String::from_utf8_lossy(&out.stdout).into_owned(),
            err: String::from_utf8_lossy(&out.stderr).into_owned(),
        }
    }

    fn calls(&self) -> String {
        std::fs::read_to_string(&self.calls).unwrap_or_default()
    }

    /// Every name in the scratch /opt still present.
    fn present(&self) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(&self.opt)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    }

    fn assert_nothing_removed(&self, text: &str) {
        for b in BACKUPS.iter().chain(KEPT.iter()) {
            assert!(
                self.opt.join(b).join("bin/boss-jobs-api").is_file(),
                "{b} was touched:\n{text}"
            );
        }
        assert!(
            !self.calls().contains("--vacuum-size"),
            "the journal was vacuumed:\n{text}"
        );
    }

    /// A refusal: exit 2, the needles in what it said, nothing removed.
    fn refused(&self, args: &[&str], extra: &[(&str, String)], needles: &[&str]) {
        let r = self.run_env(args, extra);
        let text = r.text();
        assert_eq!(r.code, 2, "{args:?} was not refused:\n{text}");
        contains_all(&text, needles, "the refusal");
        assert!(
            !r.err.contains("plan-sha256:"),
            "a refusal rendered a plan:\n{text}"
        );
        self.assert_nothing_removed(&text);
    }
}

fn contains_all(text: &str, needles: &[&str], what: &str) {
    for n in needles {
        assert!(text.contains(n), "{what}: expected `{n}` in:\n{text}");
    }
}

// ---------------------------------------------------------------------------
// The plan, and the signed write.
// ---------------------------------------------------------------------------

#[test]
fn dry_run_renders_a_plan_naming_its_own_hash_and_removes_nothing() {
    let c = Case::new("dry-run");
    let r = c.run(&["--dry-run"]);
    let text = r.text();
    assert_eq!(r.code, 0, "the dry run did not pass:\n{text}");
    c.assert_nothing_removed(&text);
    for b in BACKUPS {
        contains_all(
            &r.out,
            &[&format!("would remove {} (", c.opt.join(b).display())],
            "the plan",
        );
    }
    // Each candidate's top level is in the plan, for the approver to read.
    contains_all(
        &r.out,
        &[
            "), holding:\n  VERSION\n  bin\n",
            "would vacuum the journal to 1G",
        ],
        "the plan",
    );
    for k in KEPT {
        assert!(
            !r.out
                .contains(&format!("would remove {} (", c.opt.join(k).display())),
            "the plan names the kept {k}:\n{text}"
        );
    }
    // Free space and journal usage move on their own: stderr, not the plan.
    contains_all(
        &r.err,
        &[
            "root free before:",
            "journal: Archived and active journals take up 4.1G",
            "DRY RUN",
        ],
        "the dry run's stderr",
    );
    assert!(!r.out.contains("root free") && !r.out.contains("4.1G"));
    assert_eq!(
        r.plan_sha(),
        sha256(&r.out),
        "the named hash is not the plan's"
    );
    // Every bound ran, the unit bound included.
    assert!(
        c.calls().contains("systemctl list-units"),
        "the unit bound did not run in the dry run:\n{text}"
    );
}

#[test]
fn two_renders_of_one_state_are_byte_identical() {
    let c = Case::new("deterministic");
    let a = c.run(&["--dry-run"]);
    let b = c.run(&["--dry-run"]);
    assert_eq!(a.code, 0, "{}", a.text());
    assert_eq!(a.out, b.out, "two renders differ");
}

#[test]
fn the_signed_plan_removes_exactly_the_backups_and_vacuums_to_1g() {
    let c = Case::new("for-real");
    let plan = c.run(&["--dry-run"]);
    let sha = plan.plan_sha();
    let r = c.run(&[&sha]);
    let text = r.text();
    assert_eq!(r.code, 0, "the signed run failed:\n{text}");
    assert!(
        r.out.starts_with(&plan.out),
        "the write did not print the approved plan first:\n{text}"
    );
    let mut kept: Vec<String> = KEPT.iter().map(|s| s.to_string()).collect();
    kept.sort();
    assert_eq!(c.present(), kept, "the survivors are wrong:\n{text}");
    for b in BACKUPS {
        contains_all(
            &text,
            &[&format!("removed {}", c.opt.join(b).display())],
            "the record",
        );
    }
    assert!(
        c.calls().contains("journalctl --vacuum-size=1G"),
        "the journal was not vacuumed to 1G:\n{}",
        c.calls()
    );
    contains_all(&text, &["OK — removed 4 backup directories"], "the verdict");

    // At most once: the applied plan's backups are gone, so it no longer
    // hashes to the signature and a second run removes nothing.
    let again = c.run(&[&sha]);
    assert_eq!(
        again.code,
        2,
        "a second run was not refused:\n{}",
        again.text()
    );
    contains_all(&again.text(), &["not the approved"], "the second run");
}

#[test]
fn a_plan_that_moved_since_it_was_signed_removes_nothing() {
    let c = Case::new("moved");
    let sha = c.run(&["--dry-run"]).plan_sha();
    put(&c.opt.join("boss-dev-bak/late-arrival"), "new\n");
    let r = c.run(&[&sha]);
    let text = r.text();
    assert_eq!(r.code, 2, "a moved plan was not refused:\n{text}");
    contains_all(&text, &["not the approved", "late-arrival"], "the refusal");
    for b in BACKUPS {
        assert!(c.opt.join(b).is_dir(), "{b} was removed:\n{text}");
    }
    assert!(!c.calls().contains("--vacuum-size"));
}

// ---------------------------------------------------------------------------
// The bounds, each refusing with nothing removed.
// ---------------------------------------------------------------------------

#[test]
fn refuses_a_path_or_any_word_but_dry_run_or_a_hash() {
    let c = Case::new("mode");
    for args in [
        vec![],
        vec!["--for-real"],
        vec!["--for-real", "/var/backups/boss"],
        vec!["/var/backups/boss-cluster-pg"],
        vec!["/home/dauld"],
        vec!["--now"],
    ] {
        c.refused(&args, &[], &["--dry-run | <plan-sha256>"]);
    }
}

#[test]
fn refuses_a_symlinked_candidate_and_leaves_its_target() {
    let c = Case::new("symlink");
    let outside = c.root.join("var-backups-boss");
    put(&outside.join("second-stack.sql"), "data\n");
    std::os::unix::fs::symlink(&outside, c.opt.join("boss-binbak-evil")).unwrap();
    c.refused(&["--dry-run"], &[], &["boss-binbak-evil", "is a symlink"]);
    assert!(
        outside.join("second-stack.sql").is_file(),
        "the link's target was touched"
    );
}

/// The adversarial review's H1, measured: `cp -a` preserves mtimes, so a
/// rollback backup made TODAY from an old tree read as July to an mtime
/// bound and passed it. The bound is ctime, which a copy cannot carry.
#[test]
fn refuses_a_backup_copied_today_with_its_old_mtimes() {
    let c = Case::new("cp-a");
    for b in BACKUPS {
        std::fs::remove_dir_all(c.opt.join(b)).unwrap();
    }
    let ok = Command::new("cp")
        .arg("-a")
        .arg(c.opt.join("boss"))
        .arg(c.opt.join("boss-binbak-rollback-today"))
        .status()
        .expect("cp runs")
        .success();
    assert!(ok, "cp -a failed");
    // The copy carries July's mtimes...
    let mtime = std::fs::metadata(c.opt.join("boss-binbak-rollback-today/VERSION"))
        .unwrap()
        .modified()
        .unwrap();
    assert!(
        SystemTime::now().duration_since(mtime).unwrap().as_secs() > 50 * 86400,
        "the fixture did not copy an old mtime"
    );
    // ...and at today's clock it is refused, by ctime.
    let r = c.run_env(
        &["--dry-run"],
        &[("BOSS_RECLAIM_NOW", epoch_now().to_string())],
    );
    let text = r.text();
    assert_eq!(
        r.code, 2,
        "a backup copied today passed the age bound:\n{text}"
    );
    contains_all(
        &text,
        &[
            "boss-binbak-rollback-today",
            "changed (ctime) in the last 30 days",
        ],
        "the refusal",
    );
    assert!(c.opt.join("boss-binbak-rollback-today/VERSION").is_file());
    // The control: sixty days on, the same copy is old, and planned.
    let later = c.run(&["--dry-run"]);
    assert_eq!(later.code, 0, "{}", later.text());
    assert!(later.out.contains("boss-binbak-rollback-today"));
}

#[test]
fn refuses_a_candidate_that_is_a_checkout() {
    let c = Case::new("git");
    put(
        &c.opt.join("boss-dev-bak/src/.git/HEAD"),
        "ref: refs/heads/main\n",
    );
    c.refused(&["--dry-run"], &[], &["boss-dev-bak", ".git", "unpushed"]);
}

#[test]
fn refuses_a_backup_the_live_tree_resolves_into() {
    let c = Case::new("live-tree");
    // /opt/boss-cli is a symlink into a backup: a rollback that stayed.
    std::fs::remove_dir_all(c.opt.join("boss-cli")).unwrap();
    std::os::unix::fs::symlink(c.opt.join("boss-dev-bak"), c.opt.join("boss-cli")).unwrap();
    let r = c.run(&["--dry-run"]);
    let text = r.text();
    assert_eq!(r.code, 2, "a live backup was not refused:\n{text}");
    contains_all(&text, &["boss-cli", "boss-dev-bak", "LIVE"], "the refusal");
    assert!(c.opt.join("boss-dev-bak/bin/boss-jobs-api").is_file());
}

#[test]
fn refuses_a_backup_with_a_mount_inside_it() {
    let c = Case::new("mount");
    let inside = c.opt.join("boss-binbak-pre-latest-194355/bin");
    write_file(
        &c.mountinfo,
        &format!(
            "22 1 8:1 / / rw,relatime shared:1 - ext4 /dev/sda1 rw\n\
             41 22 8:1 /var/lib/boss {} rw,relatime shared:1 - ext4 /dev/sda1 rw\n",
            inside.display()
        ),
    );
    c.refused(&["--dry-run"], &[], &["is a mount point", "bind mount"]);
}

#[test]
fn refuses_a_backup_a_symlink_elsewhere_resolves_into() {
    let c = Case::new("link");
    std::os::unix::fs::symlink(
        c.opt
            .join("boss-binbak-pre-latest-194355/bin/boss-jobs-api"),
        c.links.join("boss-jobs-api"),
    )
    .unwrap();
    c.refused(&["--dry-run"], &[], &["still linked to"]);
}

#[test]
fn refuses_a_backup_a_running_process_executes_from() {
    let c = Case::new("proc");
    std::fs::create_dir_all(c.proc_dir.join("202")).unwrap();
    std::os::unix::fs::symlink(
        c.opt
            .join("boss-binbak-pre-pr73-0702-1801/bin/boss-jobs-api"),
        c.proc_dir.join("202/exe"),
    )
    .unwrap();
    write_file(&c.proc_dir.join("202/comm"), "boss-jobs-api\n");
    c.refused(&["--dry-run"], &[], &["process 202", "LIVE"]);
}

#[test]
fn refuses_a_backup_a_loaded_unit_names() {
    let c = Case::new("unit");
    let exec = c
        .opt
        .join("boss-dev-bak/bin/boss-jobs-api")
        .display()
        .to_string();
    c.refused(
        &["--dry-run"],
        &[("STUB_EXEC", exec)],
        &["boss-ops-runner.service", "LIVE"],
    );
}

/// The retired second stack's units are stopped and disabled — not
/// loaded — and kept on disk for a restore; a unit FILE, or a drop-in,
/// naming a backup still refuses it.
#[test]
fn refuses_a_backup_a_unit_file_on_disk_names() {
    for (name, file) in [
        ("unit-file", "boss-docs-api.service"),
        ("drop-in", "boss-views.service.d/override.conf"),
    ] {
        let c = Case::new(name);
        put(
            &c.units.join(file),
            &format!(
                "[Service]\nExecStart={}\n",
                c.opt
                    .join("boss-binbak-pre-pr5-20260701-032322/bin/boss-jobs-api")
                    .display()
            ),
        );
        c.refused(
            &["--dry-run"],
            &[],
            &["the unit file", file, "kept for a restore"],
        );
    }
}

#[test]
fn a_unit_bound_that_cannot_be_evaluated_is_a_refusal() {
    let c = Case::new("no-bus");
    c.refused(
        &["--dry-run"],
        &[("STUB_LIST_FAIL", "1".into())],
        &["could not list", "cannot be evaluated"],
    );
    c.refused(
        &["--dry-run"],
        &[("STUB_SHOW_FAIL", "1".into())],
        &["systemctl show", "cannot be judged"],
    );
}

// ---------------------------------------------------------------------------
// THROUGH THE RUNNER, with the real allowlist, as boss-gcp.
// ---------------------------------------------------------------------------

fn shipped_verbs(root: &Path) -> PathBuf {
    let dst = root.join("verbs");
    std::fs::create_dir_all(&dst).unwrap();
    for e in std::fs::read_dir(repo_root().join("infra/ops/verbs")).expect("infra/ops/verbs/") {
        let p = e.unwrap().path();
        if p.extension().is_some_and(|x| x == "json") {
            std::fs::copy(&p, dst.join(p.file_name().unwrap())).unwrap();
        }
    }
    dst
}

fn run_runner(
    c: &Case,
    verbs: &Path,
    verb: &str,
    args: &str,
) -> (String, Option<serde_json::Value>) {
    write_exec(
        &c.bin.join("curl"),
        "#!/bin/sh\n\
         for a in \"$@\"; do case \"$a\" in @*) cp \"${a#@}\" \"$STUB_PUT\"; printf 200; exit 0;; esac; done\n\
         cat \"$STUB_JOBS\"\n",
    );
    write_file(
        &c.root.join("jobs.json"),
        &format!(
            r#"{{"data":[{{"id":"aaaaaaaa-0000-4000-8000-000000000000","status":"open","metadata":{{"host":"boss-gcp","verb":"{verb}","args":{args}}},"steps":[{{"id":"s-execute","spec_slug":"execute","status":"ready","metadata":{{"authority_role":"platform-admin"}}}}]}}]}}"#
        ),
    );
    let put = c.root.join("put.json");
    let _ = std::fs::remove_file(&put);
    let mut cmd = Command::new("sh");
    cmd.arg(repo_root().join("infra/ops/ops-runner.sh"));
    c.env(&mut cmd);
    let out = cmd
        .env("HOST_ID", "boss-gcp")
        .env("BOSS_JOBS_URL", "http://sor.invalid")
        .env("OPS_VERBS_DIR", verbs)
        .env("STUB_JOBS", c.root.join("jobs.json"))
        .env("STUB_PUT", &put)
        .output()
        .expect("ops-runner.sh runs");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let meta = std::fs::read_to_string(&put)
        .ok()
        .map(|s| serde_json::from_str::<serde_json::Value>(&s).expect("PUT payload is JSON"))
        .map(|v| v["metadata"].clone());
    (text, meta)
}

/// A packet cannot hand the write a path, and a well-formed hash with no
/// passkey approval behind it does not reach the script either.
#[test]
fn the_runner_refuses_a_path_or_an_unapproved_hash() {
    if !has("jq") {
        eprintln!("skipping: the ops-runner is sh + jq and this box has no jq");
        return;
    }
    let c = Case::new("runner-refuses");
    let verbs = shipped_verbs(&c.root);
    let hash = "0".repeat(64);
    for args in [
        "[]".to_string(),
        r#"["/var/backups/boss"]"#.to_string(),
        r#"["--dry-run"]"#.to_string(),
        format!(r#"["{hash}","/home/dauld"]"#),
        format!(r#"["{hash}"]"#),
    ] {
        let (text, _meta) = run_runner(&c, &verbs, "reclaim-gcp-root", &args);
        assert!(
            !c.calls().contains("systemctl list-units"),
            "{args} reached the script:\n{text}"
        );
        c.assert_nothing_removed(&text);
    }
}

/// The plan verb is a plain read: answered on boss-gcp through the
/// runner, with the plan on the packet and nothing removed.
#[test]
fn the_runner_on_boss_gcp_answers_the_plan_verb() {
    if !has("jq") {
        eprintln!("skipping: the ops-runner is sh + jq and this box has no jq");
        return;
    }
    let c = Case::new("runner-plan");
    let verbs = shipped_verbs(&c.root);
    let (text, meta) = run_runner(&c, &verbs, "plan-a-gcp-root-reclaim", "[]");
    let meta = meta.expect("the runner completed the execute step");
    assert_eq!(
        meta["disposition"], "answered",
        "the plan verb was not answered:\n{text}"
    );
    assert_eq!(meta["exit_code"], "0", "the plan did not exit 0:\n{text}");
    let output = meta["output"].as_str().unwrap_or_default();
    contains_all(
        output,
        &["would remove", "boss-dev-bak", "plan-sha256:"],
        "the packet's output",
    );
    c.assert_nothing_removed(&text);
}
