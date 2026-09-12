//! A failed converge says WHY — `infra/forge/cluster-deploy-runner.sh`'s
//! build block, RUN against a stub `docker`, not read.
//!
//! THE DEFECT (backlog ddb0f7bd, measured 2026-09-09). The runner built
//! with `docker build -q`. Train 283 merged at 03:40:26 and its converge
//! failed four times — 03:46:38, 03:55:07, 04:04:23, 04:14:29 — and the
//! record held the Dockerfile context dump, a one-line ERROR, and
//! nothing the compiler said. Recovering even the exit code meant
//! reading an untruncated journal line by hand. Ruling out a compile
//! error took a workspace check on another box (it passed, in both
//! profiles); ruling out a full disk took a `disk-report` ops-request
//! (101 GB free). Neither question should have needed asking: the build
//! knew and was told not to speak.
//!
//! `-q` suppresses OUTPUT, not work — it saves no build time — so the
//! only thing it bought was a quiet journal, which capturing to a file
//! buys as well while keeping the evidence. Hence: always verbose into
//! a log, print the log ONLY on failure, and stamp the head so a repeat
//! failure is legible as a repeat rather than as four unrelated ones.
//!
//! The stamp is deliberately NOT a quarantine. `FAILED_FILE` holds an
//! unbootable head out of the cluster; this one only labels the
//! attempt, because the likely causes here are transient (a registry
//! fetch losing a DNS race on this LAN, memory contention with a
//! concurrent CI job) and a retry that PASSES is itself the finding.

use boss_testing::repo_root;
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;

struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn scratch(name: &str) -> Scratch {
    let p = std::env::temp_dir().join(format!("converge-build-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).expect("scratch dir");
    Scratch(p)
}

/// The build block, lifted from the runner between its two markers, so
/// the test exercises the shipped text rather than a copy of it.
fn build_block() -> String {
    let src = std::fs::read_to_string(repo_root().join("infra/forge/cluster-deploy-runner.sh"))
        .expect("the runner script is readable");
    let start = src
        .find("BUILD_FAILED_FILE=")
        .expect("the build block starts at BUILD_FAILED_FILE");
    let end = src[start..]
        .find("STAGE=\"push")
        .expect("the build block ends before the push stage");
    src[start..start + end].to_string()
}

/// Runs the block with a stub `docker` that exits `docker_rc` after
/// printing `docker_out`. Returns (exit code, stdout, stderr).
fn run(dir: &PathBuf, docker_rc: i32, docker_out: &str) -> (i32, String, String) {
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let mut f = std::fs::File::create(bin.join("docker")).unwrap();
    write!(
        f,
        "#!/bin/sh\ncat <<'OUT'\n{docker_out}\nOUT\nexit {docker_rc}\n"
    )
    .unwrap();
    drop(f);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(bin.join("docker"), std::fs::Permissions::from_mode(0o755))
            .unwrap();
    }
    // `git rev-parse HEAD` runs inside the block; a stub keeps the test
    // off any real repository.
    let mut g = std::fs::File::create(bin.join("git")).unwrap();
    write!(g, "#!/bin/sh\necho deadbeefdeadbeef\n").unwrap();
    drop(g);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(bin.join("git"), std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    // The block records its timing through run-summary.sh, the one
    // definition every unit's summary goes through; the file is where
    // ExecStopPost reads it from.
    let script = format!(
        "set -euo pipefail\nHOME={home}\nHEAD=abc1234\nREGISTRY=reg/boss\n. {lib}\n{block}",
        home = dir.display(),
        lib = repo_root().join("infra/run-summary.sh").display(),
        block = build_block()
    );
    let out = Command::new("bash")
        .arg("-c")
        .arg(script)
        .env(
            "PATH",
            format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
        )
        .env("BOSS_RUN_SUMMARY_FILE", dir.join("summary.json"))
        .current_dir(dir)
        .output()
        .expect("bash runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn a_failed_build_prints_what_the_build_said() {
    let s = scratch("fail");
    let (rc, _out, err) = run(&s.0, 2, "error[E0432]: unresolved import `boss_core::nope`");
    assert_ne!(rc, 0, "a failed build fails the run");
    assert!(
        err.contains("unresolved import"),
        "the compiler's own words reach the record: {err}"
    );
    assert!(
        err.contains("exit 2"),
        "the exit code is named rather than left to be re-derived: {err}"
    );
}

/// The half I got wrong first, so it is pinned rather than remembered.
///
/// BuildKit prints its epilogue — the whole failing RUN's Dockerfile
/// context — AFTER the step's own output, so a `tail` of the captured
/// log shows the recipe and hides the compiler. Measured 2026-09-09
/// 05:12 on the very first failure the capture ever saw: the last 80
/// lines were Dockerfile dump and one ERROR line, and the cargo error
/// was further up. That is the defect CLAUDE.md's Diagnosis section
/// names, committed inside the fix for it.
#[test]
fn the_failure_is_shown_even_when_the_epilogue_buries_it() {
    let s = scratch("buried");
    let mut out = String::from("error[E0433]: failed to resolve: use of undeclared crate\n");
    for i in 0..200 {
        out.push_str(&format!("Dockerfile epilogue line {i}\n"));
    }
    let (rc, _o, err) = run(&s.0, 2, &out);
    assert_ne!(rc, 0);
    assert!(
        err.contains("E0433"),
        "the compiler's error survives an epilogue longer than the window: {}",
        &err[err.len().saturating_sub(400)..]
    );
    assert!(
        err.contains("the failure, with context"),
        "and the reader is told it is the failure rather than a tail: {err}"
    );
}

/// The honest fallback: when nothing in the log names a failure, a
/// tail is all there is — and it must say so rather than imply a
/// diagnosis.
#[test]
fn a_log_with_no_marker_says_it_is_a_tail_not_a_diagnosis() {
    let s = scratch("nomarker");
    let mut out = String::new();
    for i in 0..50 {
        out.push_str(&format!("just some progress {i}\n"));
    }
    let (_rc, _o, err) = run(&s.0, 2, &out);
    assert!(
        err.contains("not a diagnosis"),
        "a tail presented as a tail: {err}"
    );
}

#[test]
fn a_failed_build_stamps_the_head_so_a_repeat_is_legible() {
    let s = scratch("stamp");
    let (_rc, _o, err1) = run(&s.0, 2, "boom");
    assert!(
        err1.contains("first attempt"),
        "the first failure says so: {err1}"
    );
    let stamp = s.0.join(".boss-last-build-failed");
    assert_eq!(
        std::fs::read_to_string(&stamp).unwrap().trim(),
        "abc1234",
        "the failing head is stamped"
    );
    let (_rc2, out2, err2) = run(&s.0, 2, "boom");
    assert!(
        out2.contains("RETRY") || err2.contains("RETRY"),
        "the second failure is legible AS a repeat: {out2}{err2}"
    );
}

#[test]
fn a_successful_build_says_nothing_and_clears_the_stamp() {
    let s = scratch("ok");
    let (_rc, _o, _e) = run(&s.0, 2, "boom");
    assert!(
        s.0.join(".boss-last-build-failed").exists(),
        "stamped by the failure"
    );

    let (rc, out, err) = run(
        &s.0,
        0,
        "#1 [internal] load build definition\n#42 exporting layers",
    );
    assert_eq!(rc, 0, "a good build succeeds: {err}");
    assert!(
        !out.contains("exporting layers") && !err.contains("exporting layers"),
        "a quiet journal is preserved — the build log is NOT printed on success: {out}{err}"
    );
    assert!(
        !s.0.join(".boss-last-build-failed").exists(),
        "a build that succeeds clears the stamp, so the next failure reads as a first attempt"
    );
}

/// WHERE THE MINUTES WENT is on the packet, not only in the journal.
/// Every converge closed `result=ok` and nothing else (measured
/// 2026-09-12 while sizing the converge as the second-longest stage of
/// a car's life at 10–20 min): answering "how long was the build" meant
/// 200 journal lines off the host. The build block now stamps
/// `build_s` and the head it built through run-summary.sh, so the
/// packet says how long the image took — the measurement a layer cache
/// would be judged against.
#[test]
fn a_successful_build_records_how_long_it_took_on_the_summary() {
    let s = scratch("converge-build-records-build-s");
    let (rc, out, err) = run(&s.0, 0, "#12 DONE 1.0s\nnaming to reg/boss:abc1234");
    assert_eq!(rc, 0, "stdout:\n{out}\nstderr:\n{err}");
    let summary = std::fs::read_to_string(s.0.join("summary.json"))
        .expect("the build block wrote the run summary");
    let v: serde_json::Value = serde_json::from_str(&summary).expect("summary is JSON");
    assert!(
        v.get("build_s")
            .and_then(|b| b.as_str().map(|x| x.parse::<u64>().is_ok()))
            .unwrap_or(false)
            || v.get("build_s").and_then(|b| b.as_u64()).is_some(),
        "build_s is a number of seconds: {summary}"
    );
    assert_eq!(v["build_head"], "abc1234", "{summary}");
}
