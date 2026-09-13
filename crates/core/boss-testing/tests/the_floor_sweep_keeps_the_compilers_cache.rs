//! Below the floor, `infra/forge/disk-floor-sweep.sh` prunes the
//! builder — but not the compiler's cache mounts, unless the disk is
//! truly in danger.
//!
//! Measured 2026-09-12: the image build moved its cargo target into
//! BuildKit cache mounts (feat/a-train-without-a-rust-change-ships-
//! without-a-rust-build) and the first warm converge read
//! `build_s=331` against a cold 329 — because the forge has sat under
//! its 100 G floor for days and this sweep, at :10 every hour, ran
//! `docker builder prune -af`: ALL build cache, the mounts included. A
//! cache wiped every hour is not a cache. So the builder prune below
//! the floor excludes `exec.cachemount` records, and only a HARD floor
//! (half the defended one) takes them too — stated in the log, with the
//! cache's size, so a reader of the journal knows what was spent.
//!
//! The sweep is RUN against stubs: `df` reports the free GB the case
//! needs, `docker` records every call. The CI-image pass and the
//! registry loop are given stubs that do nothing, so the run reaches
//! the builder step with nothing else moving the number.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const SWEEP: &str = "infra/forge/disk-floor-sweep.sh";

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

fn write_exec(path: &Path, body: &str) {
    std::fs::write(path, body).expect("write stub");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
}

struct Run {
    out: Output,
    docker_calls: String,
}

fn sweep(case: &str, free_gb: u64) -> Run {
    let dir = boss_testing::scratch_dir(&format!("floor-sweep-cache-{case}"));
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).expect("mkdir bin");
    write_exec(
        &bin.join("df"),
        &format!(
            "#!/usr/bin/env bash\n\
             echo 'Filesystem 1024-blocks Used Available Capacity Mounted on'\n\
             echo \"/dev/fake 1 1 $(({free_gb} * 1024 * 1024)) 50% /\"\n"
        ),
    );
    let calls = dir.join("docker-calls");
    let _ = std::fs::remove_file(&calls);
    // The rootless daemon: every call recorded; `system df` answers with
    // a build-cache line so the size can be stated; prunes succeed.
    write_exec(
        &bin.join("docker"),
        &format!(
            "#!/usr/bin/env bash\n\
             printf '%s\\n' \"$*\" >>{calls}\n\
             case \"$1 $2\" in\n\
             'system df') echo 'TYPE TOTAL ACTIVE SIZE RECLAIMABLE'; echo 'Build Cache 40 12 7.2GB 6.1GB (84%)'; exit 0 ;;\n\
             'builder prune') echo 'Total reclaimed space: 1GB'; exit 0 ;;\n\
             'image prune') echo 'Total reclaimed space: 0B'; exit 0 ;;\n\
             esac\n\
             exit 0\n",
            calls = calls.display()
        ),
    );
    // The SYSTEM daemon's CI-image pass and the registry loop: stubs that
    // answer nothing, so neither frees anything here.
    write_exec(
        &bin.join("system-docker"),
        "#!/usr/bin/env bash\ncase \"$1\" in info) echo /var/lib/docker;; images) :;; esac\nexit 0\n",
    );
    write_exec(
        &bin.join("curl"),
        "#!/usr/bin/env bash\necho '{}'\nexit 0\n",
    );
    let out = Command::new("bash")
        .arg(repo_root().join(SWEEP))
        .env(
            "PATH",
            format!(
                "{}:{}",
                bin.display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        .env("BOSS_CI_IMAGE_DOCKER", bin.join("system-docker"))
        .env("BOSS_CI_IMAGE_DAEMON_ROOT", "/var/lib/docker")
        .env("BOSS_SWEEP_CURL_CMD", bin.join("curl"))
        .env("BOSS_DISK_FLOOR_GB", "100")
        .env_remove("BOSS_JOBS_URL")
        .current_dir(repo_root())
        .output()
        .expect("run the sweep");
    Run {
        out,
        docker_calls: std::fs::read_to_string(&calls).unwrap_or_default(),
    }
}

fn log(r: &Run) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&r.out.stdout),
        String::from_utf8_lossy(&r.out.stderr)
    )
}

/// Under the floor but above the hard floor (92 G free, floor 100,
/// hard floor 50): the build cache is pruned EXCEPT the compiler's
/// mounts, and no bare `builder prune -af` runs.
#[test]
fn below_the_floor_the_compilers_cache_mounts_are_kept() {
    let r = sweep("soft", 92);
    assert!(
        r.docker_calls
            .lines()
            .any(|l| l.starts_with("builder prune") && l.contains("type!=exec.cachemount")),
        "the builder prune must exclude cache mounts:\n{}",
        r.docker_calls
    );
    assert!(
        !r.docker_calls.lines().any(|l| l == "builder prune -af"),
        "a bare prune -af wipes the compiler's cache every hour:\n{}",
        r.docker_calls
    );
    let l = log(&r);
    assert!(
        l.contains("kept the compiler's cache mounts") && l.contains("7.2GB"),
        "the log must say the mounts were kept and how big the cache is:\n{l}"
    );
}

/// Truly in danger (20 G free, hard floor 50): the mounts go too, and
/// the log says so with the size it spent.
#[test]
fn below_the_hard_floor_the_mounts_go_too_and_the_log_says_so() {
    let r = sweep("hard", 20);
    assert!(
        r.docker_calls.lines().any(|l| l == "builder prune -af"),
        "below the hard floor everything goes:\n{}",
        r.docker_calls
    );
    let l = log(&r);
    assert!(
        l.contains("HARD FLOOR") && l.contains("cache mounts"),
        "the log must name the hard floor and what it took:\n{l}"
    );
}

/// Above the floor nothing is pruned at all — unchanged behaviour.
#[test]
fn above_the_floor_nothing_is_pruned() {
    let r = sweep("above", 180);
    assert!(
        !r.docker_calls.contains("builder prune"),
        "{}",
        r.docker_calls
    );
    assert!(log(&r).contains("nothing to do"));
}
