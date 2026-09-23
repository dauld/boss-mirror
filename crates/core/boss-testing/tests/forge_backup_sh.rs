//! `infra/forge/forge-backup.sh` — the forge backs itself up.
//!
//! Backlog 121831e6 (the prerequisite of the DR rehearsal dd8120ba):
//! the repositories, Forgejo's database, the runner registration and
//! the token signing keys lived in exactly one place, the forge's root
//! disk, and nothing copied them. The script runs `forgejo dump` in the
//! `forgejo` container, keeps a bounded count of verified archives on
//! the host, and refuses loudly — a failed unit, a `failed` packet —
//! whenever it could not prove the archive it kept.
//!
//! RUN against stubs, the way the other forge scripts are tested: `df`
//! reports the free space a case needs, and `docker` answers `inspect`
//! and streams a fixture archive for `exec`, recording every call. The
//! fixture is a real tar.gz built here with the entries `forgejo dump`
//! writes (read off Forgejo's cmd/dump.go, 2026-09-23): `forgejo-db.sql`,
//! `app.ini`, `repos/<owner>/<repo>.git/…` and `data/…`. `tar` and `gzip`
//! are the real ones, because verifying the archive is the claim.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use boss_testing::{repo_root, scratch_dir, write_exec, write_file};

const SCRIPT: &str = "infra/forge/forge-backup.sh";

struct Case {
    dir: PathBuf,
    dest: PathBuf,
    summary: PathBuf,
    docker_calls: PathBuf,
}

/// What the stub `docker exec` streams.
enum Dump {
    /// A well-formed dump holding the given top-level entries.
    Archive(&'static [&'static str]),
    /// Output that never ends until the reader stops taking it.
    Endless,
    /// Half an archive, then a non-zero exit — a dump that died.
    DiesMidStream,
}

fn case(name: &str, dump: Dump, running: bool) -> Case {
    let dir = scratch_dir(&format!("forge-backup-{name}"));
    let bin = dir.join("bin");
    boss_testing::create_dir(&bin);
    let dest = dir.join("backups");
    let docker_calls = dir.join("docker-calls");

    // A fixture tree with the dump's own layout, archived without a
    // leading `./`, the way forgejo writes it.
    let tree = dir.join("tree");
    for d in [
        "repos/david/boss.git/objects",
        "data/jwt",
        "data/packages/aa",
        "custom/conf",
    ] {
        boss_testing::create_dir(&tree.join(d));
    }
    write_file(
        &tree.join("forgejo-db.sql"),
        "CREATE TABLE user (id INTEGER);\n",
    );
    write_file(
        &tree.join("app.ini"),
        "[repository]\nROOT = /data/git/repositories\n",
    );
    write_file(
        &tree.join("repos/david/boss.git/HEAD"),
        "ref: refs/heads/main\n",
    );
    write_file(&tree.join("data/jwt/private.pem"), "not a real key\n");
    write_file(&tree.join("data/packages/aa/blob"), "registry layer\n");
    write_file(&tree.join("custom/conf/app.ini"), "[server]\n");
    let fixture = dir.join("fixture.tar.gz");

    let exec_body = match dump {
        Dump::Archive(entries) => {
            let st = Command::new("tar")
                .arg("-czf")
                .arg(&fixture)
                .arg("-C")
                .arg(&tree)
                .args(entries)
                .status()
                .expect("tar the fixture");
            assert!(st.success(), "building the fixture archive failed");
            format!("cat {}", fixture.display())
        }
        Dump::Endless => "while :; do head -c 65536 /dev/zero || exit 141; done".to_string(),
        Dump::DiesMidStream => {
            let st = Command::new("tar")
                .arg("-czf")
                .arg(&fixture)
                .arg("-C")
                .arg(&tree)
                .args(["forgejo-db.sql", "app.ini", "repos", "data/jwt"])
                .status()
                .expect("tar the fixture");
            assert!(st.success());
            format!(
                "head -c 60 {}; echo 'dump: sqlite: database is locked' >&2; exit 1",
                fixture.display()
            )
        }
    };
    write_exec(
        &bin.join("docker"),
        &format!(
            "#!/usr/bin/env bash\n\
             printf '%s\\n' \"$*\" >>{calls}\n\
             case \"$1\" in\n\
             inspect) echo {running}; exit 0 ;;\n\
             exec) {exec_body} ;;\n\
             *) exit 0 ;;\n\
             esac\n",
            calls = docker_calls.display(),
        ),
    );
    Case {
        dir,
        dest,
        summary: PathBuf::new(),
        docker_calls,
    }
}

/// Run the script with `free_gb` free on the root volume and any extra
/// environment the case needs.
fn run(c: &mut Case, free_gb: u64, env: &[(&str, &str)]) -> Output {
    let bin = c.dir.join("bin");
    write_exec(
        &bin.join("df"),
        &format!(
            "#!/usr/bin/env bash\n\
             echo 'Filesystem 1K-blocks Used Available Use% Mounted on'\n\
             echo \"/dev/fake 239000000 1 $(({free_gb} * 1024 * 1024)) 50% /\"\n"
        ),
    );
    c.summary = c.dir.join("summary.json");
    let mut cmd = Command::new("bash");
    cmd.arg(repo_root().join(SCRIPT))
        .env(
            "PATH",
            format!(
                "{}:{}",
                bin.display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        .env("BOSS_FORGE_BACKUP_DIR", &c.dest)
        .env("BOSS_FORGE_BACKUP_DOCKER", bin.join("docker"))
        .env("BOSS_FORGE_BACKUP_MIN_MB", "0")
        .env("BOSS_RUN_SUMMARY_FILE", &c.summary)
        .current_dir(repo_root());
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd.output().expect("run forge-backup.sh")
}

fn log(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

fn archives(dest: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dest)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

fn summary(c: &Case) -> serde_json::Value {
    let text = std::fs::read_to_string(&c.summary)
        .unwrap_or_else(|e| panic!("the run left no summary at {}: {e}", c.summary.display()));
    serde_json::from_str(&text).expect("the summary is JSON")
}

const GOOD: &[&str] = &["forgejo-db.sql", "app.ini", "repos", "data/jwt"];

/// The common case: one dump, verified, promoted to a named archive the
/// owner alone can read (it carries app.ini's secrets and the JWT
/// signing key), recorded on the packet with what an operator needs to
/// trust it without opening it — and the offsite legs stated as ABSENT,
/// so the packet never reads as a disaster-recovery copy it is not.
#[test]
fn a_good_dump_is_verified_kept_and_recorded() {
    let mut c = case("good", Dump::Archive(GOOD), true);
    let out = run(&mut c, 120, &[]);
    assert!(out.status.success(), "{}", log(&out));

    let kept: Vec<String> = archives(&c.dest)
        .into_iter()
        .filter(|n| n.starts_with("forge-") && n.ends_with(".tar.gz"))
        .collect();
    assert_eq!(kept.len(), 1, "one archive kept: {:?}", archives(&c.dest));
    assert!(
        !archives(&c.dest).iter().any(|n| n.starts_with(".partial")),
        "no partial left behind: {:?}",
        archives(&c.dest)
    );
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(c.dest.join(&kept[0]))
        .expect("stat archive")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600, "the archive carries Forgejo's secrets");
    let latest = std::fs::read_to_string(c.dest.join(".latest")).expect(".latest");
    assert!(
        latest.trim().ends_with(&kept[0]),
        ".latest names it: {latest}"
    );

    // The dump was asked for the irreplaceable part only.
    let calls = std::fs::read_to_string(&c.docker_calls).unwrap_or_default();
    let exec = calls
        .lines()
        .find(|l| l.starts_with("exec "))
        .unwrap_or_else(|| panic!("no docker exec: {calls}"));
    for flag in [
        "-u git",
        "forgejo dump",
        "--file -",
        "--type tar.gz",
        "--skip-package-data",
        "--skip-custom-dir",
    ] {
        assert!(exec.contains(flag), "the dump call lacks {flag}: {exec}");
    }

    let s = summary(&c);
    assert_eq!(s["archive"], kept[0].as_str(), "{s}");
    assert_eq!(s["repositories"], "1", "{s}");
    assert_eq!(s["kept"], "1", "{s}");
    assert!(s["sha256"].as_str().is_some_and(|h| h.len() == 64), "{s}");
    assert!(
        s["stamp_epoch"]
            .as_str()
            .is_some_and(|e| e.parse::<u64>().is_ok()),
        "{s}"
    );
    assert!(
        s["offsite"].as_str().is_some_and(|o| o.starts_with("none")),
        "the packet must say there is no offsite copy: {s}"
    );
    assert!(log(&out).contains("offsite: NONE"), "{}", log(&out));
}

/// Below the floor the dump never starts: a backup that fills the disk
/// wedges the Forgejo it is backing up (the Actions dispatcher stopped
/// for eight hours on a full disk, 2026-09-03).
#[test]
fn below_the_disk_floor_it_refuses_before_dumping() {
    let mut c = case("floor", Dump::Archive(GOOD), true);
    let out = run(&mut c, 22, &[]);
    assert!(!out.status.success(), "{}", log(&out));
    let l = log(&out);
    assert!(l.contains("REFUSED") && l.contains("22"), "{l}");
    let calls = std::fs::read_to_string(&c.docker_calls).unwrap_or_default();
    assert!(!calls.contains("exec"), "no dump below the floor: {calls}");
    assert!(summary(&c)["refused"].as_str().is_some(), "{}", summary(&c));
}

/// A stopped Forgejo is named, not dumped from.
#[test]
fn a_stopped_container_is_refused_by_name() {
    let mut c = case("stopped", Dump::Archive(GOOD), false);
    let out = run(&mut c, 120, &[]);
    assert!(!out.status.success(), "{}", log(&out));
    assert!(log(&out).contains("forgejo"), "{}", log(&out));
    let calls = std::fs::read_to_string(&c.docker_calls).unwrap_or_default();
    assert!(!calls.contains("exec"), "{calls}");
}

/// A dump that dies mid-stream leaves a truncated archive that LOOKS
/// present — the pg backup's own lesson. It is refused, its stderr is
/// printed whole, and nothing is kept under a backup's name.
#[test]
fn a_dump_that_dies_is_refused_and_nothing_is_kept() {
    let mut c = case("dies", Dump::DiesMidStream, true);
    let out = run(&mut c, 120, &[]);
    assert!(!out.status.success(), "{}", log(&out));
    let l = log(&out);
    assert!(
        l.contains("database is locked"),
        "the dump's own words: {l}"
    );
    assert!(
        archives(&c.dest).iter().all(|n| !n.starts_with("forge-")),
        "{:?}",
        archives(&c.dest)
    );
}

/// An archive without the database is not a backup of the forge.
#[test]
fn an_archive_without_the_database_is_refused() {
    let mut c = case(
        "nodb",
        Dump::Archive(&["app.ini", "repos", "data/jwt"]),
        true,
    );
    let out = run(&mut c, 120, &[]);
    assert!(!out.status.success(), "{}", log(&out));
    assert!(log(&out).contains("forgejo-db.sql"), "{}", log(&out));
    assert!(archives(&c.dest).iter().all(|n| !n.starts_with("forge-")));
}

/// An archive without a single repository is not either.
#[test]
fn an_archive_without_a_repository_is_refused() {
    let mut c = case(
        "norepo",
        Dump::Archive(&["forgejo-db.sql", "app.ini", "data/jwt"]),
        true,
    );
    let out = run(&mut c, 120, &[]);
    assert!(!out.status.success(), "{}", log(&out));
    assert!(log(&out).contains("repositor"), "{}", log(&out));
}

/// THE 35 GB TRAP. Forgejo's image sets GITEA_CUSTOM=/data/gitea — the
/// same directory as the app data, packages included — so a dump
/// without --skip-custom-dir carries the whole registry under
/// `custom/`. An archive holding registry blobs is refused, whichever
/// entry they rode in on.
#[test]
fn an_archive_carrying_the_registry_is_refused() {
    for (name, entries) in [
        (
            "pkgs",
            &["forgejo-db.sql", "app.ini", "repos", "data/packages"][..],
        ),
        (
            "custom",
            &["forgejo-db.sql", "app.ini", "repos", "custom"][..],
        ),
    ] {
        let mut c = case(name, Dump::Archive(entries), true);
        let out = run(&mut c, 120, &[]);
        assert!(!out.status.success(), "{name}: {}", log(&out));
        assert!(archives(&c.dest).iter().all(|n| !n.starts_with("forge-")));
    }
}

/// A dump past the ceiling is cut off at the ceiling — the disk can
/// never lose more than that to one run — and refused.
#[test]
fn a_dump_past_the_ceiling_is_cut_off_and_refused() {
    let mut c = case("ceiling", Dump::Endless, true);
    let out = run(&mut c, 120, &[("BOSS_FORGE_BACKUP_MAX_MB", "1")]);
    assert!(!out.status.success(), "{}", log(&out));
    assert!(log(&out).contains("ceiling"), "{}", log(&out));
    assert!(
        archives(&c.dest)
            .iter()
            .all(|n| !n.starts_with("forge-") && !n.starts_with(".partial")),
        "{:?}",
        archives(&c.dest)
    );
}

/// A dump smaller than the least the irreplaceable part can be is a
/// dump of the wrong thing.
#[test]
fn a_dump_below_the_minimum_size_is_refused() {
    let mut c = case("tiny", Dump::Archive(GOOD), true);
    let out = run(&mut c, 120, &[("BOSS_FORGE_BACKUP_MIN_MB", "1")]);
    assert!(!out.status.success(), "{}", log(&out));
    assert!(log(&out).contains("smaller"), "{}", log(&out));
}

/// Retention is a COUNT, pruned oldest-first by the stamp in the name,
/// and every deletion is printed.
#[test]
fn retention_keeps_a_bounded_count_and_prunes_loudly() {
    let mut c = case("retain", Dump::Archive(GOOD), true);
    boss_testing::create_dir(&c.dest);
    for day in 1..=4 {
        write_file(
            &c.dest.join(format!("forge-2026090{day}-094000.tar.gz")),
            "old",
        );
    }
    // A partial a killed run left behind is swept, loudly.
    write_file(&c.dest.join(".partial-20260901-000000.tar.gz"), "torn");
    let out = run(&mut c, 120, &[("BOSS_FORGE_BACKUP_KEEP", "3")]);
    assert!(out.status.success(), "{}", log(&out));
    let names = archives(&c.dest);
    let kept: Vec<&String> = names.iter().filter(|n| n.starts_with("forge-")).collect();
    assert_eq!(kept.len(), 3, "{names:?}");
    assert!(!names.contains(&"forge-20260901-094000.tar.gz".to_string()));
    assert!(!names.contains(&"forge-20260902-094000.tar.gz".to_string()));
    assert!(names.contains(&"forge-20260904-094000.tar.gz".to_string()));
    assert!(
        !names.iter().any(|n| n.starts_with(".partial")),
        "{names:?}"
    );
    let l = log(&out);
    assert!(
        l.contains("pruned forge-20260901-094000.tar.gz")
            && l.contains("pruned forge-20260902-094000.tar.gz"),
        "{l}"
    );
    assert_eq!(summary(&c)["pruned"], "2", "{}", summary(&c));
}
