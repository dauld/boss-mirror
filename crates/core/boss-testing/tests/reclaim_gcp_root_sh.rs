//! `infra/gcp/reclaim-gcp-root.sh` is RUN, not read — against a scratch
//! `/opt`, a scratch process table, a stubbed `systemctl` and a stubbed
//! `journalctl`, so every verdict below is one the script actually
//! reached and every removal is one it actually made.
//!
//! WHY THE VERB EXISTS (backlog d3c7eada car 2, 2026-09-26). boss-gcp's
//! 48 GB root sat at 11-13 GB free for nine days against its 17 GB
//! floor. Car 1's disk-report reading found ~2.3 GB of July 2026 binary
//! backups under /opt (`boss-binbak-*`, `boss-dev-bak`) and a 4.1 GB
//! journal, beside DATA that is David's call — the cluster-pg dumps and
//! the second-stack capture under /var/backups, the home directories,
//! /usr/local, and the live /opt/boss and /opt/boss-cli. So the reclaim
//! is a MUTATING ops verb whose paths are the script's own: it removes
//! the two backup globs and vacuums the journal to a fixed bound, and
//! refuses anything else.
//!
//! What each case pins: the dry run runs every bound and removes
//! nothing; the real run removes exactly the backup directories and
//! leaves every neighbour — /opt/boss, /opt/boss-cli, a look-alike
//! name — intact; a path handed to the script, a symlinked candidate, a
//! fresh backup, and a backup that /opt/boss, a symlink, a running
//! process or a loaded unit resolves into are each refused with nothing
//! removed; and, through `ops-runner.sh` with the shipped verb files, a
//! path in the packet never reaches the script while a dry run is
//! answered on boss-gcp.

use boss_testing::{repo_root, scratch_dir, write_exec, write_file};
use std::path::{Path, PathBuf};
use std::process::Command;

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

struct Case {
    root: PathBuf,
    bin: PathBuf,
    opt: PathBuf,
    proc_dir: PathBuf,
    links: PathBuf,
    calls: PathBuf,
}

/// `write_file`, creating the parents first.
fn put(path: &Path, body: &str) {
    std::fs::create_dir_all(path.parent().expect("a parent")).unwrap();
    write_file(path, body);
}

/// Age every file and directory under `p` by 60 days, the way the July
/// backups are on the host.
fn age(p: &Path) {
    let ok = Command::new("find")
        .arg(p)
        .args(["-exec", "touch", "-h", "-d", "60 days ago", "{}", "+"])
        .status()
        .expect("find runs")
        .success();
    assert!(ok, "could not age {}", p.display());
}

impl Case {
    fn new(name: &str) -> Self {
        let root = scratch_dir(&format!("reclaim-gcp-root-{name}"));
        let bin = root.join("bin");
        let opt = root.join("opt");
        let proc_dir = root.join("proc");
        let links = root.join("links");
        for d in [&bin, &opt, &proc_dir, &links] {
            std::fs::create_dir_all(d).unwrap();
        }
        for b in BACKUPS.iter().chain(KEPT.iter()) {
            put(&opt.join(b).join("bin/boss-jobs-api"), "binary\n");
        }
        age(&opt);
        // One running process, whose binary is the live tree's.
        std::fs::create_dir_all(proc_dir.join("101")).unwrap();
        std::os::unix::fs::symlink(opt.join("boss/bin/boss-jobs-api"), proc_dir.join("101/exe"))
            .unwrap();
        write_file(&proc_dir.join("101/comm"), "boss-jobs-api\n");
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
    for a in "$@"; do u="$a"; done
    case "$u" in
      boss-ops-runner.service) echo "{ path=${STUB_EXEC:-/usr/bin/true} ; argv[]=${STUB_EXEC:-/usr/bin/true} ; }" ;;
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
            .env(
                "BOSS_RECLAIM_LINK_DIRS",
                format!("{} {}", self.opt.display(), self.links.display()),
            );
    }

    fn run(&self, args: &[&str]) -> (i32, String) {
        self.run_env(args, &[])
    }

    fn run_env(&self, args: &[&str], extra: &[(&str, String)]) -> (i32, String) {
        let mut cmd = Command::new("bash");
        cmd.arg(repo_root().join(SCRIPT)).args(args);
        self.env(&mut cmd);
        for (k, v) in extra {
            cmd.env(k, v);
        }
        let out = cmd.output().expect("reclaim-gcp-root.sh runs");
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        (out.status.code().unwrap_or(-1), text)
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
}

fn contains_all(text: &str, needles: &[&str], what: &str) {
    for n in needles {
        assert!(text.contains(n), "{what}: expected `{n}` in:\n{text}");
    }
}

#[test]
fn dry_run_runs_every_bound_and_removes_nothing() {
    let c = Case::new("dry-run");
    let (code, text) = c.run(&["--dry-run"]);
    assert_eq!(code, 0, "the dry run did not pass:\n{text}");
    c.assert_nothing_removed(&text);
    for b in BACKUPS {
        contains_all(
            &text,
            &[&format!("would remove {}", c.opt.join(b).display())],
            "the plan",
        );
    }
    for k in KEPT {
        assert!(
            !text.contains(&format!("would remove {}\n", c.opt.join(k).display()))
                && !text.contains(&format!("would remove {} (", c.opt.join(k).display())),
            "the plan names the kept {k}:\n{text}"
        );
    }
    contains_all(
        &text,
        &[
            "would vacuum the journal to 1G",
            "journal: Archived and active journals take up 4.1G",
            "root free before:",
            "DRY RUN",
        ],
        "the dry run",
    );
    // Every bound ran, the unit bound included.
    assert!(
        c.calls().contains("systemctl list-units"),
        "the unit bound did not run in the dry run:\n{text}"
    );
}

#[test]
fn the_real_run_removes_exactly_the_backups_and_vacuums_to_1g() {
    let c = Case::new("for-real");
    let (code, text) = c.run(&["--for-real"]);
    assert_eq!(code, 0, "the real run failed:\n{text}");
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
}

#[test]
fn refuses_a_path_or_any_word_but_the_two_modes() {
    let c = Case::new("mode");
    for args in [
        vec![],
        vec!["--for-real", "/var/backups/boss"],
        vec!["/var/backups/boss-cluster-pg"],
        vec!["--now"],
    ] {
        let (code, text) = c.run(&args);
        assert_eq!(code, 2, "{args:?} was not refused:\n{text}");
        assert!(text.contains("--dry-run | --for-real"), "{args:?}: {text}");
        c.assert_nothing_removed(&text);
    }
}

#[test]
fn refuses_a_symlinked_candidate_and_leaves_its_target() {
    let c = Case::new("symlink");
    let outside = c.root.join("var-backups-boss");
    put(&outside.join("second-stack.sql"), "data\n");
    std::os::unix::fs::symlink(&outside, c.opt.join("boss-binbak-evil")).unwrap();
    let (code, text) = c.run(&["--for-real"]);
    assert_eq!(code, 2, "a symlinked candidate was not refused:\n{text}");
    contains_all(&text, &["boss-binbak-evil", "is a symlink"], "the refusal");
    assert!(
        outside.join("second-stack.sql").is_file(),
        "the link's target was touched"
    );
    c.assert_nothing_removed(&text);
}

#[test]
fn refuses_a_backup_younger_than_thirty_days() {
    let c = Case::new("young");
    put(
        &c.opt.join("boss-binbak-pre-pr5-20260701-032322/bin/fresh"),
        "yesterday\n",
    );
    let (code, text) = c.run(&["--for-real"]);
    assert_eq!(code, 2, "a fresh backup was not refused:\n{text}");
    contains_all(&text, &["modified in the last 30 days"], "the refusal");
    c.assert_nothing_removed(&text);
}

#[test]
fn refuses_a_backup_the_live_tree_resolves_into() {
    let c = Case::new("live-tree");
    // /opt/boss-cli is a symlink into a backup: a rollback that stayed.
    std::fs::remove_dir_all(c.opt.join("boss-cli")).unwrap();
    std::os::unix::fs::symlink(c.opt.join("boss-dev-bak"), c.opt.join("boss-cli")).unwrap();
    let (code, text) = c.run(&["--for-real"]);
    assert_eq!(code, 2, "a live backup was not refused:\n{text}");
    contains_all(&text, &["boss-cli", "boss-dev-bak", "LIVE"], "the refusal");
    assert!(c.opt.join("boss-dev-bak/bin/boss-jobs-api").is_file());
    assert!(!c.calls().contains("--vacuum-size"));
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
    let (code, text) = c.run(&["--for-real"]);
    assert_eq!(code, 2, "a linked-to backup was not refused:\n{text}");
    contains_all(&text, &["still linked to"], "the refusal");
    c.assert_nothing_removed(&text);
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
    let (code, text) = c.run(&["--dry-run"]);
    assert_eq!(code, 2, "a running backup was not refused:\n{text}");
    contains_all(&text, &["process 202", "LIVE"], "the refusal");
    c.assert_nothing_removed(&text);
}

#[test]
fn refuses_a_backup_a_loaded_unit_names() {
    let c = Case::new("unit");
    let exec = c
        .opt
        .join("boss-dev-bak/bin/boss-jobs-api")
        .display()
        .to_string();
    let (code, text) = c.run_env(&["--for-real"], &[("STUB_EXEC", exec)]);
    assert_eq!(code, 2, "a unit's backup was not refused:\n{text}");
    contains_all(&text, &["boss-ops-runner.service", "LIVE"], "the refusal");
    c.assert_nothing_removed(&text);
}

#[test]
fn a_unit_bound_that_cannot_be_evaluated_is_a_refusal() {
    let c = Case::new("no-bus");
    let (code, text) = c.run_env(&["--dry-run"], &[("STUB_LIST_FAIL", "1".into())]);
    assert_eq!(code, 2, "an unreadable unit list passed:\n{text}");
    contains_all(&text, &["cannot be judged"], "the refusal");
    c.assert_nothing_removed(&text);
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

fn run_runner(c: &Case, verbs: &Path, args: &str) -> (String, Option<serde_json::Value>) {
    write_exec(
        &c.bin.join("curl"),
        "#!/bin/sh\n\
         for a in \"$@\"; do case \"$a\" in @*) cp \"${a#@}\" \"$STUB_PUT\"; printf 200; exit 0;; esac; done\n\
         cat \"$STUB_JOBS\"\n",
    );
    write_file(
        &c.root.join("jobs.json"),
        &format!(
            r#"{{"data":[{{"id":"aaaaaaaa-0000-4000-8000-000000000000","status":"open","metadata":{{"host":"boss-gcp","verb":"reclaim-gcp-root","args":{args}}},"steps":[{{"id":"s-execute","spec_slug":"execute","status":"ready","metadata":{{"authority_role":"platform-admin"}}}}]}}]}}"#
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

/// A packet cannot hand the verb a path: a path in place of the mode is
/// not one of the two literals, and a path after it is one arg too many.
#[test]
fn the_allowlist_refuses_a_path_in_the_packet() {
    if !has("jq") {
        eprintln!("skipping: the ops-runner is sh + jq and this box has no jq");
        return;
    }
    let c = Case::new("runner-refuses");
    let verbs = shipped_verbs(&c.root);
    for (args, needle) in [
        ("[]", "missing required arg mode"),
        (r#"["/var/backups/boss"]"#, "is not one of"),
        (r#"["--for-real","/home/dauld"]"#, "takes at most 1 arg"),
    ] {
        let (text, meta) = run_runner(&c, &verbs, args);
        let meta = meta.expect("the runner completed the execute step");
        assert_eq!(
            meta["disposition"], "refused",
            "{args} was not refused:\n{text}"
        );
        let reason = meta["reason"].as_str().unwrap_or_default();
        assert!(reason.contains(needle), "{args}: {reason}");
        c.assert_nothing_removed(&text);
    }
}

#[test]
fn the_runner_on_boss_gcp_answers_a_dry_run() {
    if !has("jq") {
        eprintln!("skipping: the ops-runner is sh + jq and this box has no jq");
        return;
    }
    let c = Case::new("runner-dry-run");
    let verbs = shipped_verbs(&c.root);
    let (text, meta) = run_runner(&c, &verbs, r#"["--dry-run"]"#);
    let meta = meta.expect("the runner completed the execute step");
    assert_eq!(
        meta["disposition"], "answered",
        "the dry run was not answered:\n{text}"
    );
    assert_eq!(
        meta["exit_code"], "0",
        "the dry run did not exit 0:\n{text}"
    );
    let output = meta["output"].as_str().unwrap_or_default();
    contains_all(
        output,
        &["DRY RUN", "would remove", "boss-dev-bak"],
        "the packet's output",
    );
    c.assert_nothing_removed(&text);
}
