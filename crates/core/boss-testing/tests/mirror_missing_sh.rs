//! `infra/forge/mirror-base-images.sh --missing` mirrors only the tags
//! the forge registry does not hold, and the converge runs it before
//! every image build.
//!
//! Measured 2026-09-13 01:20–03:20Z: car feat/the-conductor-can-launch-
//! a-gate added `COPY --from=10.20.0.15:3000/david/alpine-k8s:1.33.3`
//! to the cluster image. The mirror LIST had carried that tag since
//! 2026-09-09; the registry had never received it, because the mirror
//! runs only when a human files the verb. Six converges failed on
//! `not found`, two merged trains sat unconverged for two hours, and
//! the repair was one `mirror-base-images` ops-request. A tag in the
//! list is a declaration; the registry holding it is the fact, and the
//! converge is the place to close the gap — before the build that
//! needs it, against a stub-free `docker manifest inspect`.

use boss_testing::{repo_root, write_exec};
use std::process::Command;

/// A stub docker: `manifest inspect` answers 0 for tags listed in the
/// `present` file and 1 otherwise; pull/tag/push succeed; every call is
/// recorded.
fn run(case: &str, present: &[&str]) -> (std::process::Output, String) {
    let dir = boss_testing::scratch_dir(&format!("mirror-missing-{case}"));
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let calls = dir.join("docker-calls");
    let _ = std::fs::remove_file(&calls);
    let present_file = dir.join("present");
    std::fs::write(&present_file, present.join("\n") + "\n").unwrap();
    write_exec(
        &bin.join("docker"),
        &format!(
            "#!/usr/bin/env bash\n\
             printf '%s\\n' \"$*\" >>{calls}\n\
             case \"$1 $2\" in\n\
             'manifest inspect') grep -qxF \"$3\" {present} && exit 0 || exit 1 ;;\n\
             esac\n\
             exit 0\n",
            calls = calls.display(),
            present = present_file.display()
        ),
    );
    let out = Command::new("bash")
        .arg(repo_root().join("infra/forge/mirror-base-images.sh"))
        .arg("--missing")
        .env(
            "PATH",
            format!(
                "{}:{}",
                bin.display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        .env("BOSS_FORGE_REGISTRY_BASE", "reg.test/david")
        .output()
        .expect("bash runs the mirror script");
    (out, std::fs::read_to_string(&calls).unwrap_or_default())
}

#[test]
fn missing_mirrors_only_the_tags_the_registry_lacks() {
    // Everything present except alpine-k8s, the tag that bit.
    let listed: Vec<String> = {
        let out = Command::new("bash")
            .arg(repo_root().join("infra/forge/mirror-base-images.sh"))
            .arg("--check")
            .env("BOSS_FORGE_REGISTRY_BASE", "reg.test/david")
            .output()
            .expect("--check runs");
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|l| l.split("->").nth(1))
            .map(|s| s.trim().to_string())
            .collect()
    };
    assert!(
        listed.iter().any(|t| t.ends_with("alpine-k8s:1.33.3")),
        "{listed:?}"
    );
    let present: Vec<&str> = listed
        .iter()
        .filter(|t| !t.ends_with("alpine-k8s:1.33.3"))
        .map(String::as_str)
        .collect();
    let (out, calls) = run("one-missing", &present);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let pushes: Vec<&str> = calls.lines().filter(|l| l.starts_with("push ")).collect();
    assert_eq!(
        pushes,
        vec!["push reg.test/david/alpine-k8s:1.33.3"],
        "only the absent tag is pushed:\n{calls}"
    );
    assert!(
        calls
            .lines()
            .any(|l| l == "pull docker.io/alpine/k8s:1.33.3"),
        "the absent tag is pulled from its source:\n{calls}"
    );
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("1 image(s) mirrored") && text.contains("already in the registry"),
        "the log says what was done and what was already there:\n{text}"
    );
}

#[test]
fn missing_with_everything_present_pulls_nothing() {
    let out = Command::new("bash")
        .arg(repo_root().join("infra/forge/mirror-base-images.sh"))
        .arg("--check")
        .env("BOSS_FORGE_REGISTRY_BASE", "reg.test/david")
        .output()
        .unwrap();
    let listed: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.split("->").nth(1))
        .map(|s| s.trim().to_string())
        .collect();
    let present: Vec<&str> = listed.iter().map(String::as_str).collect();
    let (out, calls) = run("all-present", &present);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !calls.contains("pull ") && !calls.contains("push "),
        "{calls}"
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("0 image(s) mirrored"));
}

/// The converge calls it before the build, outside the lifted build
/// block, and a mirror that cannot complete does not stop the build —
/// the build names a base it lacks itself.
#[test]
fn the_converge_mirrors_what_is_missing_before_it_builds() {
    let src =
        std::fs::read_to_string(repo_root().join("infra/forge/cluster-deploy-runner.sh")).unwrap();
    let mirror = src
        .find("mirror-base-images.sh --missing")
        .expect("the runner runs mirror-base-images.sh --missing");
    let build = src.find("BUILD_FAILED_FILE=").expect("the build block");
    assert!(
        mirror < build,
        "the mirror step must come before the build block"
    );
}
