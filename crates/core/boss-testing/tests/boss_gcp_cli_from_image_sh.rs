//! `infra/gcp/install-cli-from-image.sh` is RUN, not read — against a
//! stubbed `docker` that records every pull/create/cp it receives and
//! hands back a stub `boss` which, like the image's real one, is
//! compiled from `unknown` and reads `BOSS_BUILD_COMMIT` at runtime.
//! Every verdict below is one the script actually reached.
//!
//! WHY THE STEP EXISTS (backlog 6f58e9a1, David's option (b),
//! 2026-09-15). boss-gcp's `/usr/local/bin/boss` had no refresh path:
//! only a human `deploy-services.sh prod` — a full deploy of the second
//! stack 45641c91 retires — ever installed it, and boss-gcp-converge
//! installed units only. The binary there printed `boss 0.1.0` with no
//! commit at all, older than built_from itself, so every host verb that
//! shells to the CLI refused 78 by name (ops-request 20ba7cdf) and the
//! 17 adrift workflow kinds stayed unpublished. Now the converge
//! installs the CLI from the cluster image at the sha it converged to,
//! so "the CLI on this host" is the tree's CLI by construction and a
//! read on the converge packet (`cli_sha` beside `converge_sha`).
//!
//! What each case pins: the binary lands under a per-sha generation and
//! `/usr/local/bin/boss` is a wrapper that names that sha as
//! `BOSS_BUILD_COMMIT` (the image's binary is compiled with
//! BOSS_CLI_BUILT_FROM=unknown and would otherwise say `built from
//! unknown`, which the floor check refuses); the install is CONFIRMED
//! through `boss --version` and an unconfirmed one leaves the previous
//! generation linked; a second tick at the same sha pulls nothing; a
//! failed pull is refused with docker's complete output and the
//! credential path named; the facts ride the run summary; and, driven
//! through the converge itself, the units still install and report when
//! the CLI step fails.
//!
//! Nothing here touches a host or a registry. `docker` is a stub on
//! every path.

use boss_testing::{repo_root, scratch_dir, write_exec, write_file};
use std::path::{Path, PathBuf};
use std::process::Command;

const SCRIPT: &str = "infra/gcp/install-cli-from-image.sh";
const CONVERGE: &str = "infra/gcp/boss-gcp-converge.sh";
const REPO_IMAGE: &str = "registry.invalid:3000/david/boss";

const SHA_A: &str = "aaaaaaaa1111111111111111111111111111aaaa";
const SHA_B: &str = "bbbbbbbb2222222222222222222222222222bbbb";
const SHA_C: &str = "cccccccc3333333333333333333333333333cccc";

fn has(tool: &str) -> bool {
    Command::new("sh")
        .args(["-c", &format!("command -v {tool} >/dev/null 2>&1")])
        .status()
        .is_ok_and(|s| s.success())
}

/// One fixture: a stub `docker` on PATH, a generation store, a link
/// path standing in for /usr/local/bin/boss, and a run-summary file.
struct Case {
    root: PathBuf,
    bin: PathBuf,
    store: PathBuf,
    link: PathBuf,
    summary: PathBuf,
    docker_log: PathBuf,
}

impl Case {
    fn new(name: &str) -> Self {
        let root = scratch_dir(&format!("boss-gcp-cli-{name}"));
        let bin = root.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let store = root.join("opt-boss-cli");
        let link = root.join("usr-local-bin").join("boss");
        std::fs::create_dir_all(link.parent().unwrap()).unwrap();
        let summary = root.join("summary.json");
        let docker_log = root.join("docker.log");
        // The stub docker: every call appended to a log; `pull` fails on
        // STUB_PULL_FAIL with the daemon's shape of an auth refusal;
        // `cp` hands back a `boss` that, like the image's real binary,
        // knows its commit only from BOSS_BUILD_COMMIT — or, under
        // STUB_BOSS_SAYS, names a commit the wrapper did not set, which
        // is what an install that must NOT be confirmed looks like.
        write_exec(
            &bin.join("docker"),
            r#"#!/bin/sh
echo "docker $*" >> "$STUB_DOCKER_LOG"
case "$1" in
  pull)
    if [ -n "${STUB_PULL_FAIL:-}" ]; then
      echo "Error response from daemon: Head \"http://registry.invalid:3000/v2/david/boss/manifests/x\": unauthorized: DISTINCTIVE-DOCKER-ERROR (stub)" >&2
      exit 1
    fi
    echo "Status: Downloaded newer image for $2"
    ;;
  create) echo "stubcontainer0123" ;;
  cp)
    dest="$3"
    cat > "$dest" <<'EOF'
#!/bin/sh
echo "boss 0.1.0 built from ${STUB_BOSS_SAYS:-${BOSS_BUILD_COMMIT:-unknown}}"
EOF
    chmod +x "$dest"
    ;;
  rm|rmi) ;;
  images) printf '%s\n' "${STUB_IMAGES:-}" ;;
  *) echo "stub docker: unexpected $*" >&2; exit 99 ;;
esac
exit 0
"#,
        );
        Self {
            root,
            bin,
            store,
            link,
            summary,
            docker_log,
        }
    }

    fn run(&self, sha: &str, extra: &[(&str, String)]) -> (i32, String) {
        let mut cmd = Command::new("bash");
        cmd.arg(repo_root().join(SCRIPT)).arg(sha);
        self.env(&mut cmd, extra);
        let out = cmd.output().expect("install-cli-from-image.sh runs");
        (out.status.code().unwrap_or(-1), text(&out))
    }

    fn env(&self, cmd: &mut Command, extra: &[(&str, String)]) {
        cmd.env_clear()
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    self.bin.display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env("STUB_DOCKER_LOG", &self.docker_log)
            .env("BOSS_CLI_IMAGE_REPO", REPO_IMAGE)
            .env("BOSS_CLI_STORE", &self.store)
            .env("BOSS_CLI_LINK", &self.link)
            .env("BOSS_RUN_SUMMARY_FILE", &self.summary);
        for (k, v) in extra {
            cmd.env(k, v);
        }
    }

    fn docker_calls(&self) -> Vec<String> {
        std::fs::read_to_string(&self.docker_log)
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn clear_docker_log(&self) {
        let _ = std::fs::remove_file(&self.docker_log);
    }

    fn summary(&self, key: &str) -> String {
        let s = std::fs::read_to_string(&self.summary).unwrap_or_default();
        let v: serde_json::Value = serde_json::from_str(&s).unwrap_or_default();
        v.get(key)
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string()
    }

    fn current(&self) -> Option<String> {
        std::fs::read_link(self.store.join("current"))
            .ok()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
    }

    /// What the host's PATH would answer: `boss --version` through the
    /// link, with nothing in the environment — the wrapper must supply
    /// the commit itself.
    fn version_through_link(&self) -> (i32, String) {
        let out = Command::new(&self.link)
            .arg("--version")
            .env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .output()
            .expect("the link runs");
        (out.status.code().unwrap_or(-1), text(&out))
    }
}

fn text(out: &std::process::Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn is_symlink(p: &Path) -> bool {
    std::fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_symlink())
}

#[test]
fn the_tree_sha_lands_as_a_generation_and_the_wrapper_names_it() {
    if !has("jq") {
        return;
    }
    let c = Case::new("install");
    let (rc, out) = c.run(SHA_A, &[]);
    assert_eq!(rc, 0, "{out}");

    // Per-sha generation, current linked to it, the link a wrapper.
    assert!(
        c.store.join(SHA_A).join("boss").is_file(),
        "the binary lands under the generation path: {out}"
    );
    assert_eq!(c.current().as_deref(), Some(SHA_A), "{out}");
    assert!(
        is_symlink(&c.link),
        "the link is a symlink to the wrapper: {out}"
    );
    assert_eq!(
        std::fs::read_link(&c.link).unwrap(),
        c.store.join("boss"),
        "the link resolves to the store's wrapper: {out}"
    );

    // The one thing the whole car is for: the host's `boss --version`
    // names the converged commit, with nothing set by the caller.
    let (vrc, v) = c.version_through_link();
    assert_eq!(vrc, 0, "{v}");
    assert!(
        v.contains(&format!("built from {SHA_A}")),
        "the wrapper sets BOSS_BUILD_COMMIT to the generation's sha: {v}"
    );

    // Docker was driven the extraction way: pull, create, cp, rm.
    let calls = c.docker_calls();
    let image = format!("{REPO_IMAGE}:{SHA_A}");
    assert!(
        calls.iter().any(|l| l == &format!("docker pull {image}")),
        "pulls the image tagged with the tree sha: {calls:?}"
    );
    assert!(
        calls.iter().any(|l| l == &format!("docker create {image}")),
        "{calls:?}"
    );
    assert!(
        calls
            .iter()
            .any(|l| l.starts_with("docker cp stubcontainer0123:/usr/local/bin/boss ")),
        "copies the CLI out of the created container: {calls:?}"
    );
    assert!(
        calls.iter().any(|l| l.starts_with("docker rm ")),
        "the container is removed: {calls:?}"
    );

    // And the facts ride the packet.
    assert_eq!(c.summary("cli_sha"), SHA_A);
    assert_eq!(c.summary("cli_result"), "ok");
    assert_eq!(c.summary("cli_action"), "installed");
    assert_eq!(c.summary("cli_image"), image);
    assert!(
        out.contains("CONFIRMED") && out.contains(&SHA_A[..8]),
        "the run says it confirmed, and which sha: {out}"
    );
}

#[test]
fn a_second_tick_at_the_same_sha_pulls_nothing_and_still_verifies() {
    if !has("jq") {
        return;
    }
    let c = Case::new("unchanged");
    let (rc, out) = c.run(SHA_A, &[]);
    assert_eq!(rc, 0, "{out}");
    c.clear_docker_log();
    let _ = std::fs::remove_file(&c.summary);

    let (rc, out) = c.run(SHA_A, &[]);
    assert_eq!(rc, 0, "{out}");
    let calls = c.docker_calls();
    assert!(
        !calls.iter().any(|l| l.starts_with("docker pull")),
        "a tick that has the sha already must not pull the image again every half hour: {calls:?}"
    );
    assert_eq!(c.summary("cli_action"), "unchanged", "{out}");
    assert_eq!(c.summary("cli_result"), "ok");
    assert_eq!(c.summary("cli_sha"), SHA_A);
    let (_, v) = c.version_through_link();
    assert!(v.contains(&format!("built from {SHA_A}")), "{v}");
}

#[test]
fn a_new_sha_flips_current_and_keeps_the_previous_generation_on_disk() {
    if !has("jq") {
        return;
    }
    let c = Case::new("flip");
    let (rc, out) = c.run(SHA_A, &[]);
    assert_eq!(rc, 0, "{out}");
    let (rc, out) = c.run(SHA_B, &[]);
    assert_eq!(rc, 0, "{out}");
    assert_eq!(c.current().as_deref(), Some(SHA_B), "{out}");
    assert!(
        c.store.join(SHA_A).join("boss").is_file(),
        "the previous generation is the revert target and stays: {out}"
    );
    let (_, v) = c.version_through_link();
    assert!(v.contains(&format!("built from {SHA_B}")), "{v}");
    assert_eq!(c.summary("cli_action"), "installed");

    // Old generations are pruned past the keep count; the current one
    // never is.
    let (rc, out) = c.run(SHA_C, &[("BOSS_CLI_KEEP", "2".into())]);
    assert_eq!(rc, 0, "{out}");
    assert!(
        !c.store.join(SHA_A).exists(),
        "with keep=2, the oldest of three generations is pruned: {out}"
    );
    assert!(c.store.join(SHA_B).exists() && c.store.join(SHA_C).exists());
    assert_eq!(c.current().as_deref(), Some(SHA_C));
}

/// The image's `boss` is compiled from `unknown` and says only what its
/// environment tells it; a binary that names ANOTHER commit than the
/// generation's — a wrapper defect, a wrong image, a copy that is not
/// what the tag says — must not be confirmed, and the previous
/// generation must still answer.
#[test]
fn a_binary_that_names_another_commit_is_not_confirmed_and_the_previous_generation_stays_linked() {
    if !has("jq") {
        return;
    }
    let c = Case::new("unconfirmed");
    let (rc, out) = c.run(SHA_A, &[]);
    assert_eq!(rc, 0, "{out}");
    let _ = std::fs::remove_file(&c.summary);

    let (rc, out) = c.run(SHA_B, &[("STUB_BOSS_SAYS", "deadbeef".repeat(5))]);
    assert_ne!(rc, 0, "an unconfirmed install must exit non-zero: {out}");
    assert_eq!(
        c.current().as_deref(),
        Some(SHA_A),
        "the previous generation stays linked: {out}"
    );
    let (_, v) = c.version_through_link();
    assert!(
        v.contains(&format!("built from {SHA_A}")),
        "the host's boss still answers with the last confirmed sha: {v}"
    );
    assert!(
        !c.store.join(SHA_B).exists(),
        "an unconfirmed generation does not stay on disk as if it were one: {out}"
    );
    assert!(
        out.contains("NOT CONFIRMED") && out.contains("deadbeef"),
        "the refusal says what the binary actually said: {out}"
    );
    assert_eq!(c.summary("cli_sha"), SHA_B, "the sha it TRIED is recorded");
    let result = c.summary("cli_result");
    assert!(
        result.starts_with("unconfirmed"),
        "cli_result names the verdict: {result}"
    );
    assert!(
        c.summary("anomalies").contains("NOT CONFIRMED"),
        "the reason rides the packet's anomalies, not only the journal"
    );
}

#[test]
fn a_failed_pull_is_refused_with_dockers_complete_output_and_the_credential_path_named() {
    if !has("jq") {
        return;
    }
    let c = Case::new("pull-fails");
    let (rc, out) = c.run(SHA_A, &[("STUB_PULL_FAIL", "1".into())]);
    assert_ne!(rc, 0, "{out}");
    assert!(
        out.contains("DISTINCTIVE-DOCKER-ERROR"),
        "docker's own output is printed, not reduced: {out}"
    );
    for must in ["docker login", "registry.invalid:3000", "read:package"] {
        assert!(
            out.contains(must),
            "the refusal names the credential path ({must}): {out}"
        );
    }
    assert!(c.current().is_none(), "nothing was linked: {out}");
    assert!(
        !c.link.exists(),
        "a first install that failed leaves the link alone: {out}"
    );
    let result = c.summary("cli_result");
    assert!(result.starts_with("refused"), "{result}");
    assert!(
        result.contains("pull"),
        "the result names the step: {result}"
    );
    assert_eq!(c.summary("cli_sha"), SHA_A);
    let calls = c.docker_calls();
    assert!(
        !calls.iter().any(|l| l.starts_with("docker create")),
        "nothing was created from an image that did not arrive: {calls:?}"
    );
}

#[test]
fn a_host_without_docker_is_refused_by_name() {
    if !has("jq") {
        return;
    }
    let c = Case::new("no-docker");
    std::fs::remove_file(c.bin.join("docker")).unwrap();
    // The system PATH may carry a real docker; the test must never
    // reach it. Restrict PATH to the stub dir plus what coreutils/jq
    // need, minus any directory holding a docker.
    let clean_path = std::env::var("PATH")
        .unwrap_or_default()
        .split(':')
        .filter(|d| !Path::new(d).join("docker").exists())
        .collect::<Vec<_>>()
        .join(":");
    let mut cmd = Command::new("bash");
    cmd.arg(repo_root().join(SCRIPT)).arg(SHA_A);
    c.env(&mut cmd, &[]);
    cmd.env("PATH", format!("{}:{clean_path}", c.bin.display()));
    let out = cmd.output().unwrap();
    let t = text(&out);
    assert_ne!(out.status.code(), Some(0), "{t}");
    assert!(t.contains("no docker"), "{t}");
    assert!(c.summary("cli_result").starts_with("refused"), "{t}");
    assert!(c.summary("cli_result").contains("docker"), "{t}");
}

/// The first install on a host that carries the OLD real-file binary:
/// make-before-break means that file is untouched until a generation
/// has been confirmed, and replaced by the wrapper link only then.
#[test]
fn the_old_real_file_binary_is_replaced_only_after_a_generation_is_confirmed() {
    if !has("jq") {
        return;
    }
    let c = Case::new("old-binary");
    write_exec(&c.link, "#!/bin/sh\necho 'boss 0.1.0'\n");
    let (rc, out) = c.run(SHA_A, &[("STUB_PULL_FAIL", "1".into())]);
    assert_ne!(rc, 0, "{out}");
    assert!(
        !is_symlink(&c.link),
        "the old binary is still what the host has: {out}"
    );
    let (_, v) = c.version_through_link();
    assert!(v.contains("boss 0.1.0"), "{v}");

    let (rc, out) = c.run(SHA_A, &[]);
    assert_eq!(rc, 0, "{out}");
    assert!(is_symlink(&c.link), "{out}");
    let (_, v) = c.version_through_link();
    assert!(v.contains(&format!("built from {SHA_A}")), "{v}");
}

#[test]
fn usage_is_refused_without_a_full_sha() {
    if !has("jq") {
        return;
    }
    let c = Case::new("usage");
    for bad in ["", "abc123", "not-a-sha-at-all"] {
        let (rc, out) = c.run(bad, &[]);
        assert_ne!(rc, 0, "{bad:?}: {out}");
        assert!(
            c.docker_calls().is_empty(),
            "{bad:?}: docker was driven: {out}"
        );
    }
}

// ---------------------------------------------------------------------------
// Through the converge: the units install first and report whatever the
// CLI step does; the CLI step gets the sha the tree converged to.
// ---------------------------------------------------------------------------

/// A forge fixture the converge fast-forwards from, and a clone one
/// commit behind it — the state the whole loop is about.
struct Converge {
    case: Case,
    clone: PathBuf,
    want: String,
    calls: PathBuf,
    nodes: PathBuf,
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "test")
        .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
        .env("GIT_COMMITTER_NAME", "test")
        .env("GIT_COMMITTER_EMAIL", "test@example.invalid")
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?} in {}: {}",
        dir.display(),
        text(&out)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

impl Converge {
    fn new(name: &str) -> Self {
        let case = Case::new(&format!("converge-{name}"));
        let root = &case.root;
        let home = root.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let forge = root.join("forge.git");
        git(
            root,
            &[
                "init",
                "--quiet",
                "--bare",
                "--initial-branch=main",
                forge.to_str().unwrap(),
            ],
        );
        let seed = root.join("seed");
        git(
            root,
            &[
                "clone",
                "--quiet",
                forge.to_str().unwrap(),
                seed.to_str().unwrap(),
            ],
        );
        write_file(&seed.join("file"), "one\n");
        git(&seed, &["add", "file"]);
        git(&seed, &["commit", "--quiet", "-m", "one"]);
        git(&seed, &["push", "--quiet", "origin", "main"]);
        let first = git(&seed, &["rev-parse", "HEAD"]);
        write_file(&seed.join("file"), "one\ntwo\n");
        git(&seed, &["commit", "--quiet", "-am", "two"]);
        git(&seed, &["push", "--quiet", "origin", "main"]);
        let want = git(&seed, &["rev-parse", "HEAD"]);
        let clone = root.join("clone");
        git(
            root,
            &[
                "clone",
                "--quiet",
                "--origin",
                "forge",
                forge.to_str().unwrap(),
                clone.to_str().unwrap(),
            ],
        );
        git(&clone, &["reset", "--hard", "--quiet", &first]);

        let calls = root.join("calls.log");
        write_exec(
            &case.bin.join("installer-ok"),
            "#!/usr/bin/env bash\necho \"stub installer: args=$*\" >>\"$STUB_CALLS\"\necho 'units: 6 timer unit pair(s) installed and enabled'\nexit 0\n",
        );
        let nodes = root.join("nodes.json");
        write_file(
            &nodes,
            r#"{"data":[{"id":"boss-gcp","roles":["off-cluster-observer"]}]}"#,
        );
        Self {
            case,
            clone,
            want,
            calls,
            nodes,
        }
    }

    fn run(&self, extra: &[(&str, String)]) -> (i32, String) {
        let mut cmd = Command::new("bash");
        cmd.arg(repo_root().join(CONVERGE));
        self.case.env(&mut cmd, extra);
        cmd.env("HOME", self.case.root.join("home"))
            .env("BOSS_GCP_REPO_DIR", &self.clone)
            .env(
                "BOSS_GCP_CONVERGE_INSTALLER",
                self.case.bin.join("installer-ok"),
            )
            .env("BOSS_GCP_CONVERGE_CLI_INSTALLER", repo_root().join(SCRIPT))
            .env("BOSS_NODE_ID", "boss-gcp")
            .env(
                "BOSS_ESTATE_NODES_URL",
                format!("file://{}", self.nodes.display()),
            )
            .env("BOSS_NODE_ROLES_CACHE", self.case.root.join("roles.cache"))
            .env("STUB_CALLS", &self.calls);
        let out = cmd.output().expect("boss-gcp-converge.sh runs");
        (out.status.code().unwrap_or(-1), text(&out))
    }

    fn installer_calls(&self) -> String {
        std::fs::read_to_string(&self.calls).unwrap_or_default()
    }
}

#[test]
fn the_converge_installs_the_cli_at_the_sha_it_converged_to_and_records_both() {
    if !has("jq") || !has("git") {
        return;
    }
    let cv = Converge::new("ok");
    let (rc, out) = cv.run(&[]);
    assert_eq!(rc, 0, "{out}");
    assert!(
        cv.installer_calls().contains("args=units"),
        "the units install ran: {out}"
    );
    let c = &cv.case;
    assert_eq!(c.summary("converge_sha"), cv.want, "{out}");
    assert_eq!(
        c.summary("cli_sha"),
        cv.want,
        "the CLI step is handed the sha the tree converged to: {out}"
    );
    assert_eq!(c.summary("cli_result"), "ok", "{out}");
    let (_, v) = c.version_through_link();
    assert!(
        v.contains(&format!("built from {}", cv.want)),
        "the host's boss names the converged commit: {v}"
    );
    assert!(
        c.docker_calls()
            .iter()
            .any(|l| l == &format!("docker pull {REPO_IMAGE}:{}", cv.want)),
        "{:?}",
        c.docker_calls()
    );
    assert!(
        out.contains("  cli: "),
        "the CLI step's every line is printed under its own prefix, like the installer's: {out}"
    );
}

#[test]
fn a_cli_failure_does_not_stop_the_units_converge_and_is_on_the_packet() {
    if !has("jq") || !has("git") {
        return;
    }
    let cv = Converge::new("cli-fails");
    let (rc, out) = cv.run(&[("STUB_PULL_FAIL", "1".into())]);
    assert_ne!(
        rc, 0,
        "a converge whose CLI step failed has not converged: {out}"
    );
    assert!(
        cv.installer_calls().contains("args=units"),
        "the units installed regardless: {out}"
    );
    let c = &cv.case;
    // The tree moved and the units facts were recorded before the CLI
    // step could fail.
    assert_eq!(git(&cv.clone, &["rev-parse", "HEAD"]), cv.want);
    assert_eq!(c.summary("converge_sha"), cv.want, "{out}");
    assert_eq!(c.summary("converge_remote"), "forge");
    assert_eq!(c.summary("cli_sha"), cv.want);
    assert!(
        c.summary("cli_result").starts_with("refused"),
        "{}",
        c.summary("cli_result")
    );
    assert!(
        out.contains("DISTINCTIVE-DOCKER-ERROR"),
        "docker's output reaches the journal in full: {out}"
    );
    assert!(
        out.contains("the CLI step FAILED"),
        "the converge names which step failed: {out}"
    );
}
