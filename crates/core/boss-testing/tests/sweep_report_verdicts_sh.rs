//! The two report verbs a daily sweep files for itself — `disk-report`
//! (`infra/forge/disk-report.sh`) and `conformance-report`
//! (`infra/cluster/conformance-report.sh`) — carry ONE machine-readable
//! verdict line, in one shape for both (conformance-report ends with it;
//! disk-report prints it before its slow directory lists, 8fcd25c4):
//!
//!     verdict: clean
//!     verdict: <finding>
//!
//! WHY (backlog 970c0c94, measured 2026-09-18). Every
//! `measure-*-sweep-on-inspect-ready` rule files an ops-request that
//! ANSWERS — exit 0 — but the reading landed on the ops-request's
//! `execute` step as free text with no verdict: disk-report exited 0 at
//! 68% root as it would at 99%, conformance-report exited 0 while
//! reporting one undeclared object. Nothing could complete the sweep's
//! `inspect` step by rule, so four of them sat assigned to the agent for
//! 24h with a clean reading on another packet. A verdict is what a rule
//! can read (`maintenance.sweep.judge`); the exit code stays 0 for an
//! answered report, because a finding is an answer, not a failure.
//!
//! The scripts are RUN here against stubbed commands, so every verdict
//! below is one the script actually printed. The THRESHOLD each judges
//! by is the one the estate already declares, never a new number:
//! disk-report applies the estate comparator's floor
//! (`estate_compare.rs`, pinned to this script by
//! `the_disk_report_judges_by_the_comparators_floor`), and
//! conformance-report says clean at exactly zero undeclared objects.

use boss_testing::{repo_root, write_exec};
use std::path::{Path, PathBuf};
use std::process::Command;

fn scratch(case: &str) -> PathBuf {
    boss_testing::scratch_dir(&format!("sweep-report-verdicts-{case}"))
}

/// The last `verdict:` line of an output, the way the judging handler
/// reads it — trimmed, and the LAST one, because the ops-runner merges
/// stdout and stderr into one recorded `output`.
fn verdict_of(text: &str) -> Option<&str> {
    text.lines()
        .map(str::trim)
        .rfind(|l| l.starts_with("verdict: "))
}

// ---------------------------------------------------------------------------
// disk-report
// ---------------------------------------------------------------------------

/// A stub PATH for disk-report.sh: `df` answers with the planted root
/// filesystem (KiB, the way `df -k /` prints it — the same read the
/// estate observer takes, `infra/estate/observe-host.sh`); every other
/// privileged or host-shaped command is refused or silenced so the
/// script reaches its verdict without touching this host.
fn disk_report_stubs(case: &str, disk_kb: u64, free_kb: u64) -> PathBuf {
    let dir = scratch(case);
    write_exec(
        &dir.join("df"),
        &format!(
            "#!/bin/bash\n\
             # `df -k /` — the two numbers the verdict reads; anything else is the human table.\n\
             case \"$*\" in\n\
               *-k*) printf 'Filesystem 1K-blocks Used Available Use%% Mounted on\\n/dev/root {disk_kb} 0 {free_kb} 1%% /\\n' ;;\n\
               *) printf 'Filesystem Size Used Avail Use%% Mounted on\\n/dev/root 1G 0 1G 1%% /\\n' ;;\n\
             esac\n"
        ),
    );
    write_exec(&dir.join("sudo"), "#!/bin/bash\nexit 1\n");
    write_exec(&dir.join("docker"), "#!/bin/bash\nexit 1\n");
    write_exec(&dir.join("findmnt"), "#!/bin/bash\necho ext4\n");
    write_exec(
        &dir.join("du"),
        "#!/bin/bash\nprintf '0\\t%s\\n' \"${@: -1}\"\n",
    );
    dir
}

fn run_disk_report(stubs: &Path) -> (i32, String) {
    let path = format!(
        "{}:{}",
        stubs.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let out = Command::new("bash")
        .arg(repo_root().join("infra/forge/disk-report.sh"))
        .env_clear()
        .env("PATH", path)
        .output()
        .expect("disk-report.sh runs");
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out.status.code().unwrap_or(-1), all)
}

const GIB_KB: u64 = 1_048_576;

/// A 228 GB forge with 120 GB free is above every floor the estate
/// applies (max(16, min(35% of 228 = 80, 200)) = 80): clean, exit 0.
#[test]
fn a_disk_above_the_estates_floor_reads_clean() {
    let stubs = disk_report_stubs("disk-clean", 228 * GIB_KB, 120 * GIB_KB);
    let (rc, all) = run_disk_report(&stubs);
    assert_eq!(rc, 0, "an answered report exits 0:\n{all}");
    assert_eq!(
        verdict_of(&all),
        Some("verdict: clean"),
        "the verdict line is missing or not clean:\n{all}"
    );
}

/// The same forge at 63 GB free is below its 80 GB floor — the reading
/// the estate comparator files `disk_tight` on. The finding names both
/// numbers so the inspector reads the gap off the step, and the exit
/// code is still 0: a finding is an answer.
#[test]
fn a_disk_below_the_floor_reads_tight_with_both_numbers_and_still_answers() {
    let stubs = disk_report_stubs("disk-tight", 228 * GIB_KB, 63 * GIB_KB);
    let (rc, all) = run_disk_report(&stubs);
    assert_eq!(rc, 0, "a finding is an answer, not a failure:\n{all}");
    assert_eq!(
        verdict_of(&all),
        Some("verdict: disk_tight free=63g floor=80g"),
        "the finding must carry the free space and the floor it fell under:\n{all}"
    );
}

/// The absolute minimum rules on a small disk: a 40 GB host at 15 GB
/// free is 37% free — above the percentage — and still tight, because
/// 16 GiB is the least any host may hold.
#[test]
fn the_absolute_minimum_rules_on_a_small_disk() {
    let stubs = disk_report_stubs("disk-small", 40 * GIB_KB, 15 * GIB_KB);
    let (_, all) = run_disk_report(&stubs);
    assert_eq!(
        verdict_of(&all),
        Some("verdict: disk_tight free=15g floor=16g"),
        "{all}"
    );
}

/// The percentage is capped on a big disk: a 929 GB build node at 250
/// GB free is 27% — under 35% — and CLEAN, because above 200 GB the
/// percentage is not consulted (estate_compare.rs, 8e425862).
#[test]
fn the_percentage_is_capped_by_the_headroom_ceiling_on_a_big_disk() {
    let stubs = disk_report_stubs("disk-big", 929 * GIB_KB, 250 * GIB_KB);
    let (_, all) = run_disk_report(&stubs);
    assert_eq!(verdict_of(&all), Some("verdict: clean"), "{all}");
}

/// A `df` that answers nothing usable is NOT a clean disk: the verdict
/// says the disk went unmeasured, which the judging rule reads as a
/// finding and leaves for the inspector.
#[test]
fn an_unmeasured_disk_is_not_a_clean_one() {
    let stubs = disk_report_stubs("disk-unmeasured", 0, 0);
    write_exec(
        &stubs.join("df"),
        "#!/bin/bash\necho 'df: cannot read table of mounted file systems' >&2\nexit 1\n",
    );
    let (rc, all) = run_disk_report(&stubs);
    assert_eq!(rc, 0, "{all}");
    let v = verdict_of(&all).unwrap_or_else(|| panic!("no verdict line:\n{all}"));
    assert!(
        v.starts_with("verdict: disk_unmeasured"),
        "an unreadable df must read as unmeasured, never clean: {v}\n{all}"
    );
}

/// The verdict is printed BEFORE the long per-directory lists, and only
/// once (backlog 8fcd25c4, 2026-09-26). On boss-gcp the one-level-down
/// scans outlasted the runner's 30 s default and the kill landed before
/// a verdict printed LAST; a verdict read from `df -k /` first costs
/// nothing, and the lists are what a kill may cut. Exactly one line, so
/// the judge's "last `verdict:` line" read finds this one and no other.
#[test]
fn the_verdict_is_printed_once_and_before_the_directory_lists() {
    let stubs = disk_report_stubs("disk-verdict-first", 228 * GIB_KB, 120 * GIB_KB);
    let (rc, all) = run_disk_report(&stubs);
    assert_eq!(rc, 0, "{all}");
    let lines: Vec<&str> = all.lines().collect();
    let verdicts: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, l)| l.trim().starts_with("verdict: "))
        .map(|(i, _)| i)
        .collect();
    assert_eq!(
        verdicts.len(),
        1,
        "exactly one verdict line, at lines {verdicts:?}:\n{all}"
    );
    let lists = lines
        .iter()
        .position(|l| l.starts_with("== directories, largest first"))
        .unwrap_or_else(|| panic!("the directory list header is gone:\n{all}"));
    assert!(
        verdicts[0] < lists,
        "the verdict (line {}) must come before the directory lists (line {lists}), so a kill mid-scan cannot lose it:\n{all}",
        verdicts[0]
    );
}

// ---------------------------------------------------------------------------
// disk-report on boss-gcp
// ---------------------------------------------------------------------------
//
// WHY (backlog d3c7eada, 2026-09-26). boss-gcp's 48 GB root has sat at
// 11-13 GB free for nine days, under its 17 GB floor, and the estate
// alarm now files it. The only disk verb serving that host was `df`, so
// what fills the root could not be read through a door. The verb now
// serves boss-gcp, and the script reads the places a retired stack
// leaves residue there (/var/backups, /opt, /home, /usr/local, /var/lib)
// and marks a directory that is its OWN filesystem, because boss-gcp's
// builds (/var/lib/boss-build, sdc) and postgres (sdb) are mounted under
// /var/lib and would otherwise read as root usage.

/// The disk-report stubs, with `du` also recording every path it was
/// asked about, one per line, in `du.log` beside the stubs.
fn disk_report_stubs_logging_du(case: &str) -> (PathBuf, PathBuf) {
    let stubs = disk_report_stubs(case, 48 * GIB_KB, 12 * GIB_KB);
    let log = stubs.join("du.log");
    write_exec(
        &stubs.join("du"),
        &format!(
            "#!/bin/bash\nfor a in \"$@\"; do case \"$a\" in -*) ;; *) printf '%s\\n' \"$a\" >> '{}'; printf '0\\t%s\\n' \"$a\";; esac; done\n",
            log.display()
        ),
    );
    (stubs, log)
}

/// Every one of the places the triage named that exists on the host
/// running the test is measured, and each of /var/backups, /opt, /home
/// and /var/lib that has entries is also measured one level down — the
/// level the reclaim will be decided at.
#[test]
fn the_report_measures_the_residue_directories_and_one_level_below_them() {
    let (stubs, log) = disk_report_stubs_logging_du("disk-residue-dirs");
    let (rc, all) = run_disk_report(&stubs);
    assert_eq!(rc, 0, "{all}");
    let asked = std::fs::read_to_string(&log).unwrap_or_default();
    let asked: Vec<&str> = asked.lines().collect();
    let mut checked = 0;
    for dir in ["/var/backups", "/opt", "/home", "/usr/local", "/var/lib"] {
        if !Path::new(dir).exists() {
            continue;
        }
        checked += 1;
        assert!(
            asked.contains(&dir),
            "{dir} exists here and the report never measured it; du was asked: {asked:?}\n{all}"
        );
        // /usr/local is measured whole; the other four one level down too.
        if dir == "/usr/local" {
            continue;
        }
        let first_child = std::fs::read_dir(dir)
            .ok()
            .and_then(|mut entries| entries.next())
            .and_then(Result::ok)
            .map(|e| e.path().display().to_string());
        if let Some(child) = first_child {
            assert!(
                asked.iter().any(|a| a.starts_with(&format!("{dir}/"))),
                "{dir} has entries (e.g. {child}) and none was measured one level down: {asked:?}\n{all}"
            );
        }
    }
    assert!(
        checked > 0,
        "none of the residue directories exists on this host — the test proved nothing"
    );
}

/// A directory that is a mount point is a SEPARATE filesystem: its size
/// says nothing about the root the alarm is about, so the line says so.
#[test]
fn a_mount_point_is_marked_as_not_the_root_filesystem() {
    let (stubs, _) = disk_report_stubs_logging_du("disk-mountpoint");
    // Every path answers "I am a mount point"; findmnt names the source.
    write_exec(&stubs.join("mountpoint"), "#!/bin/bash\nexit 0\n");
    let (rc, all) = run_disk_report(&stubs);
    assert_eq!(rc, 0, "{all}");
    let lib_lines: Vec<&str> = all.lines().filter(|l| l.contains("\t/var/lib/")).collect();
    assert!(
        !lib_lines.is_empty(),
        "no /var/lib entry was listed:\n{all}"
    );
    for l in &lib_lines {
        assert!(
            l.contains("its own filesystem, not the root"),
            "a mount point under /var/lib must be marked as its own filesystem: {l}\n{all}"
        );
    }
}

/// The verb serves boss-gcp as well as the forge — the only way the
/// host's root can be measured through a door.
#[test]
fn the_disk_report_verb_serves_boss_gcp_and_the_forge() {
    let verb: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(repo_root().join("infra/ops/verbs/disk-report.json"))
            .expect("the verb file is readable"),
    )
    .expect("the verb file is JSON");
    let hosts: Vec<&str> = verb["hosts"]
        .as_array()
        .map(|a| a.iter().filter_map(|h| h.as_str()).collect())
        .unwrap_or_default();
    assert!(
        hosts.contains(&"boss-gcp") && hosts.contains(&"forge"),
        "disk-report must serve boss-gcp and forge: {hosts:?}"
    );
    assert!(
        !verb["about"].as_str().unwrap_or("").contains("MUTATING"),
        "disk-report stays read-only: {verb}"
    );
    assert_eq!(
        verb["params"],
        serde_json::json!([]),
        "no arguments: {verb}"
    );
}

// ---------------------------------------------------------------------------
// conformance-report
// ---------------------------------------------------------------------------

/// The wrapper's ONE input is the derivation it wraps — `infra/cluster/
/// undeclared-objects.sh --list`, whose contract (stdout is the orphan
/// set, one per line; exit 4 is "cannot answer" with stdout empty) is
/// pinned in `undeclared_objects_sh.rs`. Here the derivation is stubbed
/// to answer each of those shapes, so what is under test is the verdict
/// the wrapper draws from the answer.
fn stub_derivation(case: &str, body: &str) -> PathBuf {
    let dir = scratch(case);
    let stub = dir.join("undeclared-objects-stub");
    write_exec(&stub, body);
    stub
}

fn run_conformance_report(derivation: &Path) -> (i32, String, String) {
    let out = Command::new("bash")
        .arg(repo_root().join("infra/cluster/conformance-report.sh"))
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("BOSS_UNDECLARED_OBJECTS", derivation)
        .output()
        .expect("conformance-report.sh runs");
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let all = format!("{stdout}{}", String::from_utf8_lossy(&out.stderr));
    (out.status.code().unwrap_or(-1), stdout, all)
}

#[test]
fn zero_undeclared_objects_reads_clean() {
    let d = stub_derivation(
        "conf-clean",
        "#!/bin/bash\necho '0 undeclared object(s)' >&2\nexit 0\n",
    );
    let (rc, _, all) = run_conformance_report(&d);
    assert_eq!(rc, 0, "{all}");
    assert_eq!(verdict_of(&all), Some("verdict: clean"), "{all}");
}

/// One orphan is a finding that counts itself, and the orphan set the
/// derivation printed still rides the output above it — the inspector
/// reads WHAT is undeclared, not only how many.
#[test]
fn an_undeclared_object_is_a_counted_finding_that_still_answers() {
    let d = stub_derivation(
        "conf-orphan",
        "#!/bin/bash\nprintf 'Service\\tboss\\tghost-svc\\nConfigMap\\tboss\\tstray\\n'\necho '2 undeclared object(s)' >&2\nexit 0\n",
    );
    let (rc, stdout, all) = run_conformance_report(&d);
    assert_eq!(rc, 0, "a finding is an answer, not a failure:\n{all}");
    assert_eq!(
        verdict_of(&all),
        Some("verdict: 2 undeclared objects"),
        "{all}"
    );
    assert!(
        stdout.contains("Service\tboss\tghost-svc") && stdout.contains("ConfigMap\tboss\tstray"),
        "the orphan set must still ride the output:\n{all}"
    );
    // Singular when it is one — the finding is prose the inspector reads.
    let d = stub_derivation(
        "conf-one",
        "#!/bin/bash\nprintf 'Service\\tboss\\tghost-svc\\n'\nexit 0\n",
    );
    let (_, _, all) = run_conformance_report(&d);
    assert_eq!(
        verdict_of(&all),
        Some("verdict: 1 undeclared object"),
        "{all}"
    );
}

/// The derivation's refusal (exit 4, stdout empty, reason on stderr) is
/// NOT a clean cluster and not a count either: the verdict says the
/// question went unanswered, the reason rides with it, and the exit
/// code is passed through so the verb's contract ("exit 4 is cannot
/// answer") holds unchanged.
#[test]
fn a_refusal_to_answer_is_neither_clean_nor_a_count() {
    let d = stub_derivation(
        "conf-refused",
        "#!/bin/bash\necho 'CANNOT ANSWER: manifest svc-b.yaml would not parse' >&2\nexit 4\n",
    );
    let (rc, stdout, all) = run_conformance_report(&d);
    assert_eq!(rc, 4, "the derivation's exit code passes through:\n{all}");
    let named: Vec<&str> = stdout
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with("verdict: "))
        .collect();
    assert!(
        named.is_empty(),
        "a refusal names no orphan: {named:?}\n{all}"
    );
    let v = verdict_of(&all).unwrap_or_else(|| panic!("no verdict line:\n{all}"));
    assert!(
        v.starts_with("verdict: unanswered"),
        "a refusal must read as unanswered, never clean: {v}\n{all}"
    );
    assert!(
        all.contains("svc-b.yaml"),
        "the derivation's own reason must survive:\n{all}"
    );
}

/// The verb file names the wrapper, not the derivation directly — the
/// verdict is drawn by the wrapper, so an ops-request for
/// `conformance-report` must run it.
#[test]
fn the_conformance_report_verb_runs_the_wrapper_that_draws_the_verdict() {
    let verb: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(repo_root().join("infra/ops/verbs/conformance-report.json"))
            .expect("the verb file is readable"),
    )
    .expect("the verb file is JSON");
    assert_eq!(
        verb["argv"][0].as_str(),
        Some("infra/cluster/conformance-report.sh"),
        "conformance-report must run the wrapper that appends the verdict: {verb}"
    );
    let disk: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(repo_root().join("infra/ops/verbs/disk-report.json"))
            .expect("the verb file is readable"),
    )
    .expect("the verb file is JSON");
    assert_eq!(
        disk["argv"][0].as_str(),
        Some("infra/forge/disk-report.sh"),
        "disk-report's verdict lives in the script the verb already runs: {disk}"
    );
}
