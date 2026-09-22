//! `infra/estate/install-cli-from-image.sh` is RUN, not read — against a
//! stubbed `curl` that serves a small but REAL OCI image out of fixture
//! files (a token endpoint, an index, a platform manifest and gzip-tar
//! layer blobs), and records every URL it is asked for. The image's
//! `usr/local/bin/boss` is a stub which, like the real binary, is
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
//! WHY CURL AND NOT DOCKER (the rebuild of car c142457c, 2026-09-16).
//! The first build shelled to `docker pull/create/cp`. Measured
//! 2026-09-16 00:30Z, ops-request 546c13fc: boss-gcp has no docker and
//! David has said it gets none; and the image tag it pulled was the
//! FULL 40-char sha while the registry tags every image with the
//! deploy runner's 7-char short sha (tags/list: 00444a6, 0d54622, …),
//! so every pull would have been a 404. The forge package is public
//! and its token endpoint hands out a pull token with no credentials,
//! so the pull is curl + tar. Both defects are pinned here: the tag in
//! the URL is the short sha (and the full sha is never requested), and
//! nothing on PATH but curl, jq, tar, gzip and sha256sum is needed.
//!
//! What each case pins: the binary lands under a per-FULL-sha
//! generation and `/usr/local/bin/boss` is a wrapper that names that
//! sha as `BOSS_BUILD_COMMIT`; the install is CONFIRMED through
//! `boss --version` and an unconfirmed one leaves the previous
//! generation linked; a second tick at the same sha fetches nothing; a
//! tag the registry does not have is refused naming the URL and the
//! HTTP code with the body printed whole; a blob whose bytes do not
//! hash to the manifest's digest is refused; a layer above the binary
//! that whites it out is refused; a layer that is not gzip tar is
//! refused by media type; an unreachable token endpoint is refused
//! naming its URL; the facts ride the run summary; and, driven through
//! the converge itself, the units still install and report when the
//! CLI step fails.
//!
//! WHY IT LIVES UNDER infra/estate/ (backlog 9f00a805, consolidation
//! H8, car 1). Measured 2026-09-18 on #448: infra/forge/*.sh is 32
//! scripts / 9,790 lines, the largest of them shell twins of CLI verbs
//! (run-car-probe.sh for `boss prove --from-car`, tenant-census.sh for
//! `boss tenant`, …), each with its own pin — because the forge had no
//! `boss` binary. boss-gcp had solved exactly that on 2026-09-15, so the
//! installer moved from infra/gcp/ to the directory the roles installer
//! reads (infra/estate/roles.toml, node-roles.sh), and BOTH converges
//! call it: boss-gcp's unchanged, and the forge's install.sh for the
//! `cluster-operator` role — the host cluster management runs on, whose
//! tooling (talosctl, kubectl) the CLI joins. The forge cases at the end
//! pin that path: the sha the converge hands over is installed and
//! recorded as `cli_sha`; a tag the deploy runner has not built YET is
//! `not yet`, exit 0 — the forge converges every ten minutes and builds
//! the image on the same host a few minutes after each train, so a red
//! there would be a red on every train; and a real refusal still reds
//! the converge with the units installed and reported.
//!
//! Nothing here touches a host or a registry. `curl` is a stub on
//! every path.

use boss_testing::{create_dir, repo_root, scratch_dir, write_exec, write_file};
use std::path::{Path, PathBuf};
use std::process::Command;

const SCRIPT: &str = "infra/estate/install-cli-from-image.sh";
const CONVERGE: &str = "infra/gcp/boss-gcp-converge.sh";
const REGISTRY_HOST: &str = "registry.invalid:3000";
const REPO_IMAGE: &str = "registry.invalid:3000/david/boss";
const GZIP: &str = "application/vnd.oci.image.layer.v1.tar+gzip";

const SHA_A: &str = "aaaaaaaa1111111111111111111111111111aaaa";
const SHA_B: &str = "bbbbbbbb2222222222222222222222222222bbbb";
const SHA_C: &str = "cccccccc3333333333333333333333333333cccc";
/// A sha the registry has no tag for.
const SHA_D: &str = "dddddddd4444444444444444444444444444dddd";

fn has(tool: &str) -> bool {
    Command::new("sh")
        .args(["-c", &format!("command -v {tool} >/dev/null 2>&1")])
        .status()
        .is_ok_and(|s| s.success())
}

fn tools() -> bool {
    ["jq", "tar", "gzip", "sha256sum"].iter().all(|t| has(t))
}

fn sha256_of(path: &Path) -> String {
    let out = Command::new("sha256sum").arg(path).output().unwrap();
    let hex = String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .next()
        .unwrap()
        .to_string();
    format!("sha256:{hex}")
}

/// How the fixture image is shaped: the base layer always carries
/// `usr/local/bin/boss`; the top layer is a small unrelated file, or a
/// whiteout for the binary, or is declared with a non-gzip media type.
#[derive(Clone, Copy, Default)]
struct Image {
    top_whiteout: bool,
    top_zstd: bool,
}

/// One fixture: a stub `curl` on PATH serving an OCI image from files,
/// a generation store, a link path standing in for /usr/local/bin/boss,
/// and a run-summary file.
struct Case {
    root: PathBuf,
    bin: PathBuf,
    fix: PathBuf,
    store: PathBuf,
    link: PathBuf,
    summary: PathBuf,
    curl_log: PathBuf,
}

impl Case {
    fn new(name: &str) -> Self {
        Self::with_image(name, Image::default())
    }

    fn with_image(name: &str, image: Image) -> Self {
        let root = scratch_dir(&format!("install-cli-{name}"));
        let bin = root.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let fix = root.join("registry");
        std::fs::create_dir_all(fix.join("blobs")).unwrap();
        std::fs::create_dir_all(fix.join("manifests")).unwrap();
        let store = root.join("opt-boss-cli");
        let link = root.join("usr-local-bin").join("boss");
        std::fs::create_dir_all(link.parent().unwrap()).unwrap();
        let summary = root.join("summary.json");
        let curl_log = root.join("curl.log");

        // --- the image, as files ------------------------------------------
        // Layer 0 (base): usr/local/bin/boss — a stub that, like the
        // image's real binary, knows its commit only from
        // BOSS_BUILD_COMMIT; or, under STUB_BOSS_SAYS, names a commit
        // the wrapper did not set (an install that must NOT confirm).
        let base = root.join("layer-base");
        create_dir(&base.join("usr/local/bin"));
        write_exec(
            &base.join("usr/local/bin/boss"),
            "#!/bin/sh\necho \"boss 0.1.0 built from ${STUB_BOSS_SAYS:-${BOSS_BUILD_COMMIT:-unknown}}\"\n",
        );
        let base_blob = tar_gz(&base, &["usr/local/bin/boss"], &fix);
        // Layer 1 (top): unrelated, or the whiteout that deletes the
        // binary from every layer below it.
        let top = root.join("layer-top");
        let top_member = if image.top_whiteout {
            "usr/local/bin/.wh.boss"
        } else {
            "etc/motd"
        };
        create_dir(top.join(top_member).parent().unwrap());
        write_file(&top.join(top_member), "");
        let top_blob = tar_gz(&top, &[top_member], &fix);
        let top_type = if image.top_zstd {
            "application/vnd.oci.image.layer.v1.tar+zstd"
        } else {
            GZIP
        };
        let manifest = serde_json::json!({
            "schemaVersion": 2,
            "mediaType": "application/vnd.oci.image.manifest.v1+json",
            "config": {"mediaType": "application/vnd.oci.image.config.v1+json", "digest": "sha256:0000000000000000000000000000000000000000000000000000000000000000", "size": 2},
            "layers": [
                {"mediaType": GZIP, "digest": base_blob.0, "size": base_blob.1},
                {"mediaType": top_type, "digest": top_blob.0, "size": top_blob.1},
            ]
        });
        let manifest_path = root.join("manifest.json");
        write_file(&manifest_path, &manifest.to_string());
        let mdigest = sha256_of(&manifest_path);
        let msize = std::fs::metadata(&manifest_path).unwrap().len();
        std::fs::copy(&manifest_path, fix.join("manifests").join(&mdigest)).unwrap();
        // The index the tag resolves to: buildx's shape — one platform
        // manifest and one unknown/unknown attestation.
        let index = serde_json::json!({
            "schemaVersion": 2,
            "mediaType": "application/vnd.oci.image.index.v1+json",
            "manifests": [
                {"mediaType": "application/vnd.oci.image.manifest.v1+json", "digest": mdigest, "size": msize, "platform": {"architecture": "amd64", "os": "linux"}},
                {"mediaType": "application/vnd.oci.image.manifest.v1+json", "digest": "sha256:1111111111111111111111111111111111111111111111111111111111111111", "size": 564, "platform": {"architecture": "unknown", "os": "unknown"}}
            ]
        });
        write_file(&fix.join("index.json"), &index.to_string());
        write_file(
            &fix.join("token.json"),
            r#"{"token":"stub-anonymous-pull-token"}"#,
        );
        // The tags the registry knows: the deploy runner's short shas.
        write_file(
            &fix.join("tags"),
            &format!("{}\n{}\n{}\n", &SHA_A[..7], &SHA_B[..7], &SHA_C[..7]),
        );

        // --- the stub curl -------------------------------------------------
        // Reads the request shape the script uses (-o, -D, -H, the URL
        // last), logs the URL, and answers from the fixture the way the
        // forge registry does: /v2/ is 401 with a Www-Authenticate
        // naming the token realm; the token endpoint answers with no
        // credentials; manifests and blobs need the bearer token.
        // STUB_TOKEN_DOWN makes the token endpoint unreachable (curl
        // exit 7); STUB_CORRUPT_BLOB serves every blob with bytes
        // appended, so no blob hashes to its digest.
        write_exec(
            &bin.join("curl"),
            r#"#!/usr/bin/env bash
out=/dev/null; hdr=/dev/null; auth=""; url=""
while [ $# -gt 0 ]; do
  case "$1" in
    -o) out="$2"; shift ;;
    -D) hdr="$2"; shift ;;
    -H) case "$2" in Authorization:*) auth="$2" ;; esac; shift ;;
    --max-time|-w) shift ;;
    -*) ;;
    *) url="$1" ;;
  esac
  shift
done
echo "$url" >> "$STUB_CURL_LOG"
: > "$hdr"
path="${url#*://}"; path="${path#*/}"
case "$path" in
  v2/)
    printf 'HTTP/1.1 401 Unauthorized\r\nWww-Authenticate: Bearer realm="http://registry.invalid:3000/v2/token",service="container_registry"\r\n\r\n' > "$hdr"
    printf '{"errors":[{"code":"UNAUTHORIZED"}]}' > "$out"; printf 401; exit 0 ;;
  v2/token?*)
    if [ -n "${STUB_TOKEN_DOWN:-}" ]; then
      echo "curl: (7) Failed to connect to registry.invalid port 3000: DISTINCTIVE-CONNECTION-REFUSED (stub)" >&2
      printf 000; exit 7
    fi
    cp "$STUB_REGISTRY/token.json" "$out"; printf 200; exit 0 ;;
esac
if [ "$auth" != "Authorization: Bearer stub-anonymous-pull-token" ]; then
  printf '{"errors":[{"code":"UNAUTHORIZED","message":"no bearer token (stub)"}]}' > "$out"; printf 401; exit 0
fi
case "$path" in
  v2/david/boss/manifests/sha256:*)
    d="${path##*/}"
    if [ -f "$STUB_REGISTRY/manifests/$d" ]; then cp "$STUB_REGISTRY/manifests/$d" "$out"; printf 200; exit 0; fi ;;
  v2/david/boss/manifests/*)
    tag="${path##*/}"
    if grep -qx -- "$tag" "$STUB_REGISTRY/tags"; then
      printf 'HTTP/1.1 200 OK\r\nContent-Type: application/vnd.oci.image.index.v1+json\r\n\r\n' > "$hdr"
      cp "$STUB_REGISTRY/index.json" "$out"; printf 200; exit 0
    fi ;;
  v2/david/boss/blobs/sha256:*)
    d="${path##*/}"
    if [ -f "$STUB_REGISTRY/blobs/$d" ]; then
      cp "$STUB_REGISTRY/blobs/$d" "$out"
      [ -n "${STUB_CORRUPT_BLOB:-}" ] && echo corrupt >> "$out"
      if [ -n "${STUB_VANISHING_BLOB:-}" ]; then rm -f "$out"; printf 200; exit 0; fi
      printf 200; exit 0
    fi ;;
esac
printf '{"errors":[{"code":"NOT_FOUND","message":"DISTINCTIVE-REGISTRY-404 for %s (stub)"}]}' "$path" > "$out"
printf 404; exit 0
"#,
        );
        Self {
            root,
            bin,
            fix,
            store,
            link,
            summary,
            curl_log,
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
            .env("STUB_CURL_LOG", &self.curl_log)
            .env("STUB_REGISTRY", &self.fix)
            .env("BOSS_CLI_IMAGE_REPO", REPO_IMAGE)
            .env("BOSS_CLI_PLATFORM", "amd64/linux")
            .env("BOSS_CLI_STORE", &self.store)
            .env("BOSS_CLI_LINK", &self.link)
            .env("BOSS_RUN_SUMMARY_FILE", &self.summary);
        for (k, v) in extra {
            cmd.env(k, v);
        }
    }

    /// Every URL the stub curl was asked for, in order.
    fn requests(&self) -> Vec<String> {
        std::fs::read_to_string(&self.curl_log)
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn clear_requests(&self) {
        let _ = std::fs::remove_file(&self.curl_log);
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

/// A gzip tar of the named members under `dir`, stored in the fixture
/// registry under its own digest; returns (digest, size).
fn tar_gz(dir: &Path, members: &[&str], fix: &Path) -> (String, u64) {
    let tmp = fix.join(format!(
        "layer-{}.tgz",
        dir.file_name().unwrap().to_string_lossy()
    ));
    let out = Command::new("tar")
        .arg("-czf")
        .arg(&tmp)
        .arg("--owner=0")
        .arg("--group=0")
        .arg("-C")
        .arg(dir)
        .args(members)
        .output()
        .unwrap();
    assert!(out.status.success(), "tar: {}", text(&out));
    let digest = sha256_of(&tmp);
    let size = std::fs::metadata(&tmp).unwrap().len();
    std::fs::rename(&tmp, fix.join("blobs").join(&digest)).unwrap();
    (digest, size)
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
    if !tools() {
        return;
    }
    let c = Case::new("install");
    let (rc, out) = c.run(SHA_A, &[]);
    assert_eq!(rc, 0, "{out}");

    // Per-FULL-sha generation, current linked to it, the link a wrapper.
    assert!(
        c.store.join(SHA_A).join("boss").is_file(),
        "the binary lands under the generation path named by the full sha: {out}"
    );
    assert!(
        c.store.join(SHA_A).join("manifest.json").is_file(),
        "the manifest that named the layers stays beside the binary: {out}"
    );
    assert!(
        !c.store.join(SHA_A).join("pull").exists(),
        "the HTTP transcripts (token included) do not stay in the generation: {out}"
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
    // names the converged commit — the FULL sha — with nothing set by
    // the caller.
    let (vrc, v) = c.version_through_link();
    assert_eq!(vrc, 0, "{v}");
    assert!(
        v.contains(&format!("built from {SHA_A}")),
        "the wrapper sets BOSS_BUILD_COMMIT to the generation's full sha: {v}"
    );

    // And the facts ride the packet: the full sha, the image by its
    // short tag.
    let image = format!("{REPO_IMAGE}:{}", &SHA_A[..7]);
    assert_eq!(c.summary("cli_sha"), SHA_A);
    assert_eq!(c.summary("cli_result"), "ok");
    assert_eq!(c.summary("cli_action"), "installed");
    assert_eq!(c.summary("cli_image"), image);
    assert!(
        out.contains("CONFIRMED") && out.contains(&SHA_A[..8]),
        "the run says it confirmed, and which sha: {out}"
    );
}

/// Defect (2) of the first build: it pulled `<repo>:<full sha>` while
/// the deploy runner tags every image with `git rev-parse --short`
/// (cluster-deploy-runner.sh: "the short tag stays the image name, the
/// full sha is the attestation") — measured 2026-09-16 00:30Z against
/// tags/list, every tag is 7 characters.
#[test]
fn the_image_is_fetched_by_its_short_tag_and_the_full_sha_is_never_requested() {
    if !tools() {
        return;
    }
    let c = Case::new("short-tag");
    let (rc, out) = c.run(SHA_A, &[]);
    assert_eq!(rc, 0, "{out}");
    let reqs = c.requests();
    let short = format!(
        "http://{REGISTRY_HOST}/v2/david/boss/manifests/{}",
        &SHA_A[..7]
    );
    assert!(
        reqs.contains(&short),
        "the tag in the manifest URL is the 7-char short sha: {reqs:?}"
    );
    assert!(
        !reqs
            .iter()
            .any(|u| u.contains(&format!("manifests/{SHA_A}"))),
        "the full sha is the attestation, never the tag: {reqs:?}"
    );
    // The registry was driven the way an anonymous pull is: /v2/, the
    // token realm with the pull scope and no credentials, the index,
    // the platform manifest by digest, then blobs by digest.
    assert_eq!(reqs[0], format!("http://{REGISTRY_HOST}/v2/"), "{reqs:?}");
    assert_eq!(
        reqs[1],
        format!(
            "http://{REGISTRY_HOST}/v2/token?service=container_registry&scope=repository:david/boss:pull"
        ),
        "the token realm and service come from the registry's own Www-Authenticate: {reqs:?}"
    );
    assert!(
        reqs.iter().any(|u| u.starts_with(&format!(
            "http://{REGISTRY_HOST}/v2/david/boss/manifests/sha256:"
        ))),
        "the amd64/linux manifest is fetched by the digest the index names: {reqs:?}"
    );
    assert!(
        reqs.iter().any(|u| u.starts_with(&format!(
            "http://{REGISTRY_HOST}/v2/david/boss/blobs/sha256:"
        ))),
        "{reqs:?}"
    );
    assert!(
        !out.contains("docker"),
        "no docker anywhere in a successful run's account of itself: {out}"
    );
}

#[test]
fn a_second_tick_at_the_same_sha_fetches_nothing_and_still_verifies() {
    if !tools() {
        return;
    }
    let c = Case::new("unchanged");
    let (rc, out) = c.run(SHA_A, &[]);
    assert_eq!(rc, 0, "{out}");
    c.clear_requests();
    let _ = std::fs::remove_file(&c.summary);

    let (rc, out) = c.run(SHA_A, &[]);
    assert_eq!(rc, 0, "{out}");
    assert!(
        c.requests().is_empty(),
        "a tick that has the sha already must not pull the image again every half hour: {:?}",
        c.requests()
    );
    assert_eq!(c.summary("cli_action"), "unchanged", "{out}");
    assert_eq!(c.summary("cli_result"), "ok");
    assert_eq!(c.summary("cli_sha"), SHA_A);
    let (_, v) = c.version_through_link();
    assert!(v.contains(&format!("built from {SHA_A}")), "{v}");
}

#[test]
fn a_new_sha_flips_current_and_keeps_the_previous_generation_on_disk() {
    if !tools() {
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
    if !tools() {
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
        !c.store.join(format!(".staging-{SHA_B}")).exists(),
        "staging is removed on refusal: {out}"
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

/// A tag the registry does not have — the deploy runner builds the
/// image a few minutes after a train lands — is `not yet`, exit 75,
/// naming the URL and the HTTP code with the registry's body printed
/// whole. A distinct exit, since 2026-09-18 (9f00a805): on the forge
/// the converge that installs the CLI runs on the host that builds the
/// image, so this is the ordinary state of the first tick after every
/// train, and the caller decides whether it is red (boss-gcp, every
/// half hour) or a wait for the next tick (the forge).
#[test]
fn a_tag_the_registry_lacks_is_not_yet_naming_the_url_and_the_http_code() {
    if !tools() {
        return;
    }
    let c = Case::new("no-tag");
    let (rc, out) = c.run(SHA_D, &[]);
    assert_eq!(rc, 75, "not yet is exit 75, distinct from a refusal: {out}");
    let murl = format!(
        "http://{REGISTRY_HOST}/v2/david/boss/manifests/{}",
        &SHA_D[..7]
    );
    assert!(out.contains(&murl), "the refusal names the URL: {out}");
    assert!(out.contains("HTTP 404"), "and the code: {out}");
    assert!(
        out.contains("DISTINCTIVE-REGISTRY-404"),
        "the registry's body is printed, not reduced: {out}"
    );
    assert!(
        !out.contains("docker login") && !out.contains("daemon.json"),
        "nothing about a credential or a daemon — there is none: {out}"
    );
    assert!(c.current().is_none(), "nothing was linked: {out}");
    assert!(
        !c.link.exists(),
        "a first install that failed leaves the link alone: {out}"
    );
    let result = c.summary("cli_result");
    assert!(
        result.starts_with("not yet"),
        "the verdict is not yet, never refused: {result}"
    );
    assert!(
        result.contains("HTTP 404") && result.contains(&murl),
        "the packet's cli_result carries the URL and the code: {result}"
    );
    assert_eq!(c.summary("cli_sha"), SHA_D);
    assert!(
        !c.requests().iter().any(|u| u.contains("/blobs/")),
        "no blob is fetched for an image that does not exist: {:?}",
        c.requests()
    );
}

#[test]
fn an_unreachable_token_endpoint_is_refused_naming_its_url() {
    if !tools() {
        return;
    }
    let c = Case::new("token-down");
    let (rc, out) = c.run(SHA_A, &[("STUB_TOKEN_DOWN", "1".into())]);
    assert_ne!(rc, 0, "{out}");
    let turl = format!(
        "http://{REGISTRY_HOST}/v2/token?service=container_registry&scope=repository:david/boss:pull"
    );
    assert!(
        out.contains(&turl),
        "the refusal names the token URL: {out}"
    );
    assert!(
        out.contains("curl exited 7") && out.contains("DISTINCTIVE-CONNECTION-REFUSED"),
        "curl's own exit and message are printed: {out}"
    );
    let result = c.summary("cli_result");
    assert!(
        result.starts_with("refused") && result.contains(&turl),
        "{result}"
    );
    assert!(c.current().is_none() && !c.link.exists(), "{out}");
    assert!(
        !c.requests().iter().any(|u| u.contains("/manifests/")),
        "nothing past the token is asked for: {:?}",
        c.requests()
    );
}

/// A blob whose bytes do not hash to the digest the manifest names is
/// not read at all — tar never sees it — and nothing is installed.
#[test]
fn a_blob_that_does_not_match_its_digest_is_refused_before_tar_reads_it() {
    if !tools() {
        return;
    }
    let c = Case::new("digest-mismatch");
    let (rc, out) = c.run(SHA_A, &[("STUB_CORRUPT_BLOB", "1".into())]);
    assert_ne!(rc, 0, "{out}");
    assert!(
        out.contains("digest mismatch"),
        "the refusal names the verdict: {out}"
    );
    assert!(
        out.contains("sha256:") && out.contains("/blobs/sha256:"),
        "and the digest expected plus the URL it came from: {out}"
    );
    assert!(c.current().is_none() && !c.link.exists(), "{out}");
    assert!(
        !c.store.join(SHA_A).exists() && !c.store.join(format!(".staging-{SHA_A}")).exists(),
        "nothing from an untrusted image stays on disk: {out}"
    );
    let result = c.summary("cli_result");
    assert!(
        result.starts_with("refused") && result.contains("digest mismatch"),
        "{result}"
    );
}

/// A fetch that reports success and leaves no bytes is NOT a digest
/// mismatch, and must not say it is.
///
/// MEASURED IN PRODUCTION 2026-09-20 (backlog 112b1d87). A layer fetch
/// answered HTTP 200 with rc 0 and produced no file
/// (`layer.blob: No such file or directory`); the script hashed the
/// absent file, `sha256sum` wrote its complaint to stderr and nothing
/// to stdout, and the run refused with "blob digest mismatch … the
/// bytes fetched hash to sha256:" — an EMPTY actual hash.
///
/// The wrong verdict is the defect, not the failure. A digest mismatch
/// reads as corruption or a tampered registry and sends its reader to
/// the wrong investigation; the real condition was transient and the
/// identical call installed cleanly two minutes later. So the refusal
/// must name no-bytes, and must NOT be a mismatch.
#[test]
fn a_fetch_that_leaves_no_bytes_is_refused_as_no_bytes_not_as_a_mismatch() {
    if !tools() {
        return;
    }
    let c = Case::new("vanishing-blob");
    let (rc, out) = c.run(SHA_A, &[("STUB_VANISHING_BLOB", "1".into())]);

    // 75, not 1: this condition is RETRYABLE and the exit code is what
    // a caller reads. The production instance installed cleanly two
    // minutes later, so a fatal verdict would have been wrong twice.
    assert_eq!(rc, 75, "a transient no-bytes fetch is `not yet`: {out}");

    // THE VERDICT, not the prose. The refusal's explanation says the
    // words "not a digest mismatch", so asserting on the whole output
    // would match its own disclaimer; `cli_result` is the one line a
    // reader and the run summary both take as the answer.
    let result = c.summary("cli_result");
    assert!(
        result.starts_with("not yet") && result.contains("no bytes"),
        "the verdict names what actually happened: {result}"
    );
    assert!(
        !out.contains("blob digest mismatch"),
        "and the mismatch refusal — the one that reads as corruption or a tampered \
         registry — must not fire when no bytes arrived: {out}"
    );
    // The DECLARED size is named, not asserted as a literal: the
    // fixture's layer differs by a byte or two between runs, so a
    // hardcoded count here would be a flake rather than a check.
    assert!(
        out.contains("the manifest says is") && out.contains("/blobs/sha256:"),
        "it still names the size the manifest declared and the URL asked: {out}"
    );
    assert!(
        c.current().is_none() && !c.link.exists(),
        "nothing is installed: {out}"
    );
}

/// A layer ABOVE the one carrying the binary that whites it out means
/// the image has no /usr/local/bin/boss at runtime; the lower copy is
/// not installed as if it did.
#[test]
fn a_whiteout_in_a_higher_layer_is_refused_rather_than_the_deleted_copy_installed() {
    if !tools() {
        return;
    }
    let c = Case::with_image(
        "whiteout",
        Image {
            top_whiteout: true,
            ..Image::default()
        },
    );
    let (rc, out) = c.run(SHA_A, &[]);
    assert_ne!(rc, 0, "{out}");
    assert!(
        out.contains("whiteout") && out.contains("usr/local/bin/.wh.boss"),
        "the refusal names the whiteout entry and its layer: {out}"
    );
    assert!(c.current().is_none() && !c.link.exists(), "{out}");
    assert!(!c.store.join(SHA_A).exists(), "{out}");
    let result = c.summary("cli_result");
    assert!(
        result.starts_with("refused") && result.contains("whiteout"),
        "{result}"
    );
}

#[test]
fn a_layer_that_is_not_gzip_tar_is_refused_by_media_type_without_fetching_it() {
    if !tools() {
        return;
    }
    let c = Case::with_image(
        "zstd",
        Image {
            top_zstd: true,
            ..Image::default()
        },
    );
    let (rc, out) = c.run(SHA_A, &[]);
    assert_ne!(rc, 0, "{out}");
    assert!(
        out.contains("application/vnd.oci.image.layer.v1.tar+zstd")
            && out.contains("not a gzip tar"),
        "the refusal names the media type: {out}"
    );
    assert!(
        !c.requests().iter().any(|u| u.contains("/blobs/")),
        "a layer this script cannot read is not downloaded: {:?}",
        c.requests()
    );
    assert!(c.current().is_none() && !c.link.exists(), "{out}");
    let result = c.summary("cli_result");
    assert!(
        result.starts_with("refused") && result.contains("+zstd"),
        "{result}"
    );
}

#[test]
fn a_host_without_curl_is_refused_by_name() {
    if !tools() {
        return;
    }
    let c = Case::new("no-curl");
    std::fs::remove_file(c.bin.join("curl")).unwrap();
    // The system PATH carries a real curl beside bash and coreutils; the
    // test must never reach the curl and must keep the rest. Every PATH
    // directory holding a curl is replaced by a mirror of symlinks to
    // everything in it BUT curl.
    let mirrors = c.root.join("path-without-curl");
    let clean_path = std::env::var("PATH")
        .unwrap_or_default()
        .split(':')
        .filter(|d| !d.is_empty())
        .enumerate()
        .map(|(i, d)| {
            let dir = Path::new(d);
            if !dir.join("curl").exists() {
                return d.to_string();
            }
            let mirror = mirrors.join(i.to_string());
            std::fs::create_dir_all(&mirror).unwrap();
            for entry in std::fs::read_dir(dir).unwrap().flatten() {
                if entry.file_name() != "curl" {
                    let _ =
                        std::os::unix::fs::symlink(entry.path(), mirror.join(entry.file_name()));
                }
            }
            mirror.to_string_lossy().into_owned()
        })
        .collect::<Vec<_>>()
        .join(":");
    let mut cmd = Command::new("bash");
    cmd.arg(repo_root().join(SCRIPT)).arg(SHA_A);
    c.env(&mut cmd, &[]);
    cmd.env("PATH", format!("{}:{clean_path}", c.bin.display()));
    let out = cmd.output().unwrap();
    let t = text(&out);
    assert_ne!(out.status.code(), Some(0), "{t}");
    assert!(t.contains("no curl"), "{t}");
    assert!(
        t.contains("no docker"),
        "the refusal says docker is not what is missing: {t}"
    );
    assert!(c.summary("cli_result").starts_with("refused"), "{t}");
    assert!(c.summary("cli_result").contains("curl"), "{t}");
}

/// The first install on a host that carries the OLD real-file binary:
/// make-before-break means that file is untouched until a generation
/// has been confirmed, and replaced by the wrapper link only then.
#[test]
fn the_old_real_file_binary_is_replaced_only_after_a_generation_is_confirmed() {
    if !tools() {
        return;
    }
    let c = Case::new("old-binary");
    write_exec(&c.link, "#!/bin/sh\necho 'boss 0.1.0'\n");
    let (rc, out) = c.run(SHA_A, &[("STUB_TOKEN_DOWN", "1".into())]);
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

/// The argument is the FULL sha — the attestation, the generation's
/// name, what `boss --version` must print. A short sha is not enough
/// to attest with, even though the tag is derived from it.
#[test]
fn usage_is_refused_without_a_full_sha() {
    if !tools() {
        return;
    }
    let c = Case::new("usage");
    for bad in ["", "abc123", &SHA_A[..7], "not-a-sha-at-all"] {
        let (rc, out) = c.run(bad, &[]);
        assert_ne!(rc, 0, "{bad:?}: {out}");
        assert!(
            c.requests().is_empty(),
            "{bad:?}: the registry was asked: {out}"
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
        // The registry has the image for the commit the tree will
        // converge to — under the deploy runner's short tag.
        let tags = case.fix.join("tags");
        let mut have = std::fs::read_to_string(&tags).unwrap();
        have.push_str(&want[..7]);
        have.push('\n');
        write_file(&tags, &have);

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
            // The address file the converge renders before it installs
            // anything: into the scratch root here, never /etc.
            .env("BOSS_GCP_CONVERGE_SOR_ENV", self.case.root.join("sor.env"))
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
    if !tools() || !has("git") {
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
        "the CLI step is handed the FULL sha the tree converged to: {out}"
    );
    assert_eq!(c.summary("cli_result"), "ok", "{out}");
    assert_eq!(
        c.summary("cli_image"),
        format!("{REPO_IMAGE}:{}", &cv.want[..7]),
        "and pulls the image by its short tag: {out}"
    );
    let (_, v) = c.version_through_link();
    assert!(
        v.contains(&format!("built from {}", cv.want)),
        "the host's boss names the converged commit: {v}"
    );
    assert!(
        c.requests().contains(&format!(
            "http://{REGISTRY_HOST}/v2/david/boss/manifests/{}",
            &cv.want[..7]
        )),
        "{:?}",
        c.requests()
    );
    assert!(
        out.contains("  cli: "),
        "the CLI step's every line is printed under its own prefix, like the installer's: {out}"
    );
}

#[test]
fn a_cli_failure_does_not_stop_the_units_converge_and_is_on_the_packet() {
    if !tools() || !has("git") {
        return;
    }
    let cv = Converge::new("cli-fails");
    let (rc, out) = cv.run(&[("STUB_TOKEN_DOWN", "1".into())]);
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
        out.contains("DISTINCTIVE-CONNECTION-REFUSED") && out.contains("/v2/token"),
        "curl's output and the URL reach the journal in full: {out}"
    );
    assert!(
        out.contains("the CLI step FAILED"),
        "the converge names which step failed: {out}"
    );
}

/// boss-gcp's converge is UNCHANGED by the not-yet exit (9f00a805 car
/// 1 moved the installer and retargeted nothing else): a tag the
/// registry lacks is still a failed converge there, healed by its next
/// half-hourly tick, and the packet says which state it is in.
#[test]
fn the_gcp_converge_still_reds_on_a_tag_the_registry_lacks() {
    if !tools() || !has("git") {
        return;
    }
    let cv = Converge::new("not-yet");
    // Forget the image for the commit the tree converges to.
    write_file(&cv.case.fix.join("tags"), "");
    let (rc, out) = cv.run(&[]);
    assert_ne!(rc, 0, "{out}");
    assert!(cv.installer_calls().contains("args=units"), "{out}");
    assert_eq!(cv.case.summary("cli_sha"), cv.want);
    assert!(
        cv.case.summary("cli_result").starts_with("not yet"),
        "{}",
        cv.case.summary("cli_result")
    );
    assert!(out.contains("the CLI step FAILED"), "{out}");
}

// ---------------------------------------------------------------------------
// Through the forge's installer (backlog 9f00a805, consolidation H8,
// car 1): infra/forge/install.sh installs the CLI for the
// cluster-operator role, at the sha forge-converge hands it, from the
// registry the rendered address file names — and never reds the
// converge for an image the deploy runner has not built yet.
// ---------------------------------------------------------------------------

const FORGE_INSTALL: &str = "infra/forge/install.sh";
const FORGE_CONVERGE: &str = "infra/forge/forge-converge.sh";

/// What the forge declares in infra/estate/estate.toml, and therefore
/// what its converge hands its installer. Both roles matter to this
/// file: `cluster-operator` is what brings the CLI these tests are
/// about, and since 2026-09-22 (backlog cb9eb0f2) `ops-runner` is what
/// installs the ops runner — until then the runner landed on every
/// host regardless, which is what made the declaration decorative.
const FORGE_ROLES: &str = "cluster-operator,ops-runner";

/// The forge installer, driven into a scratch root the way
/// infra/lint/forge-install-covers-the-ops-runner.sh drives it: a stub
/// systemctl, no kubectl/talosctl download, the address file rendered
/// into scratch — plus this file's stub registry on PATH and the
/// generation store under scratch.
struct ForgeInstall {
    case: Case,
    etc: PathBuf,
    sor_env: PathBuf,
}

impl ForgeInstall {
    fn new(name: &str) -> Self {
        let case = Case::new(&format!("forge-{name}"));
        let etc = case.root.join("etc-systemd");
        std::fs::create_dir_all(&etc).unwrap();
        write_exec(
            &case.bin.join("systemctl"),
            "#!/usr/bin/env bash\necho \"systemctl $*\" >>\"$STUB_SYSTEMCTL_LOG\"\n[ \"${1:-}\" = is-active ] && echo active\nexit 0\n",
        );
        let sor_env = case.root.join("sor.env");
        Self { case, etc, sor_env }
    }

    /// Run install.sh with the given roles and converged sha (`None`
    /// leaves BOSS_CONVERGE_SHA unset — a hand run outside the
    /// converge).
    fn run(&self, roles: &str, sha: Option<&str>, extra: &[(&str, String)]) -> (i32, String) {
        let mut cmd = Command::new("bash");
        cmd.arg(repo_root().join(FORGE_INSTALL));
        self.case.env(&mut cmd, extra);
        // The registry host is NOT named here: the installer must take
        // it from the address file install.sh renders, the way the
        // boss-gcp installer does (forge-defaults.sh over sor.sh).
        cmd.env_remove("BOSS_CLI_IMAGE_REPO");
        cmd.env("HOME", self.case.root.join("home"))
            .env("INSTALL_ETC", &self.etc)
            .env("INSTALL_SYSTEMCTL", self.case.bin.join("systemctl"))
            .env("INSTALL_KUBECTL", "0")
            .env("INSTALL_TALOSCTL", "0")
            .env("INSTALL_SOR_ENV", &self.sor_env)
            .env("BOSS_SOR_ENV", &self.sor_env)
            .env("BOSS_NODE_ROLES", roles)
            .env("STUB_SYSTEMCTL_LOG", self.case.root.join("systemctl.log"));
        if let Some(sha) = sha {
            cmd.env("BOSS_CONVERGE_SHA", sha);
        }
        let out = cmd.output().expect("install.sh runs");
        (out.status.code().unwrap_or(-1), text(&out))
    }

    /// The registry host the rendered address file names — the one
    /// tree source, read back rather than spelled here.
    fn registry_host(&self) -> String {
        std::fs::read_to_string(&self.sor_env)
            .unwrap_or_default()
            .lines()
            .find_map(|l| l.strip_prefix("BOSS_FORGE_REGISTRY_HOST="))
            .map(str::to_string)
            .unwrap_or_default()
    }

    fn units_installed(&self) -> bool {
        self.etc.join("forge-converge.service").is_file()
    }

    /// Asked separately from the unit pairs, because it is answered by a
    /// different declaration: the ops runner lands only where the host's
    /// roles name `ops-runner` (backlog cb9eb0f2). A host that is not
    /// one still converges every unit above it.
    fn runner_installed(&self) -> bool {
        self.etc.join("boss-ops-runner.service").is_file()
    }
}

#[test]
fn the_forge_installs_the_cli_for_cluster_operator_at_the_converged_sha_from_the_registry_the_address_file_names()
 {
    if !tools() {
        return;
    }
    let f = ForgeInstall::new("ok");
    let (rc, out) = f.run(FORGE_ROLES, Some(SHA_A), &[]);
    assert_eq!(rc, 0, "{out}");
    assert!(f.units_installed(), "the units converge as before: {out}");
    assert!(
        f.runner_installed(),
        "the forge declares ops-runner, so its converge installs one: {out}"
    );
    let c = &f.case;
    assert_eq!(
        c.summary("cli_sha"),
        SHA_A,
        "cli_sha on the forge's converge packet is the sha it was handed: {out}"
    );
    assert_eq!(c.summary("cli_result"), "ok", "{out}");
    assert_eq!(c.summary("cli_action"), "installed", "{out}");
    let host = f.registry_host();
    assert!(
        !host.is_empty(),
        "the rendered address file names the registry host"
    );
    assert_eq!(
        c.summary("cli_image"),
        format!("{host}/david/boss:{}", &SHA_A[..7]),
        "the image repo comes from the rendered address file, never a literal: {out}"
    );
    assert!(
        c.requests()
            .iter()
            .any(|u| u == &format!("http://{host}/v2/david/boss/manifests/{}", &SHA_A[..7])),
        "the registry the address file names is the one asked: {:?}",
        c.requests()
    );
    let (_, v) = c.version_through_link();
    assert!(
        v.contains(&format!("built from {SHA_A}")),
        "the forge's boss names the converged commit: {v}"
    );
    assert!(
        out.contains("  cli: "),
        "the CLI step's every line is printed under its own prefix: {out}"
    );
}

#[test]
fn a_host_without_the_role_installs_no_cli_and_says_so() {
    if !tools() {
        return;
    }
    let f = ForgeInstall::new("no-role");
    let (rc, out) = f.run("off-cluster-observer", Some(SHA_A), &[]);
    assert_eq!(rc, 0, "{out}");
    assert!(f.units_installed(), "{out}");
    assert!(
        f.case.requests().is_empty(),
        "no registry request without the role: {:?}",
        f.case.requests()
    );
    assert!(!f.case.link.exists(), "{out}");
    assert!(
        out.contains("cluster-operator not among this host's roles"),
        "{out}"
    );
    // And neither role is declared here, so no ops runner either — the
    // same reading of the same list, one installer down (cb9eb0f2).
    assert!(
        !f.runner_installed(),
        "a host declaring only off-cluster-observer was given an ops runner: {out}"
    );
    assert!(
        out.contains("does not declare the ops-runner role"),
        "and the run says why it installed none: {out}"
    );
}

/// The ordinary state of the first tick after a train: the checkout is
/// at the merge, the deploy runner on this same host has not pushed the
/// image for it yet. Not a red — the converge says so, records it, and
/// the next tick retries. The previous CLI stays.
#[test]
fn an_image_the_deploy_runner_has_not_built_yet_is_not_a_red_converge_on_the_forge() {
    if !tools() {
        return;
    }
    let f = ForgeInstall::new("not-yet");
    let (rc, out) = f.run(FORGE_ROLES, Some(SHA_A), &[]);
    assert_eq!(rc, 0, "{out}");
    let _ = std::fs::remove_file(&f.case.summary);

    let (rc, out) = f.run(FORGE_ROLES, Some(SHA_D), &[]);
    assert_eq!(
        rc, 0,
        "a tag the deploy runner has not built yet must not red the forge converge: {out}"
    );
    assert!(f.units_installed(), "{out}");
    let c = &f.case;
    assert_eq!(
        c.summary("cli_sha"),
        SHA_D,
        "the sha it tried is recorded: {out}"
    );
    assert!(
        c.summary("cli_result").starts_with("not yet"),
        "{}",
        c.summary("cli_result")
    );
    assert!(
        c.summary("cli_result").contains("HTTP 404"),
        "the URL and the code ride the packet: {}",
        c.summary("cli_result")
    );
    assert!(
        out.contains("not in the registry yet") && out.contains("next tick"),
        "the installer says what it is waiting for: {out}"
    );
    assert!(
        !out.contains("FAILED"),
        "nothing about a failure — this is a wait, not a fault: {out}"
    );
    assert_eq!(
        c.current().as_deref(),
        Some(SHA_A),
        "the previous generation stays linked: {out}"
    );
    let (_, v) = c.version_through_link();
    assert!(v.contains(&format!("built from {SHA_A}")), "{v}");
}

/// A REAL refusal — the registry unreachable, a digest mismatch — is
/// still a failed converge on the forge, as on boss-gcp: the units are
/// installed and reported first, the CLI step's exit rides the packet.
#[test]
fn a_real_cli_refusal_still_reds_the_forge_converge_with_the_units_installed() {
    if !tools() {
        return;
    }
    let f = ForgeInstall::new("refused");
    let (rc, out) = f.run(FORGE_ROLES, Some(SHA_A), &[("STUB_TOKEN_DOWN", "1".into())]);
    assert_ne!(rc, 0, "{out}");
    assert!(f.units_installed(), "the units installed regardless: {out}");
    assert!(
        std::fs::read_to_string(f.case.root.join("systemctl.log"))
            .unwrap_or_default()
            .contains("enable --now forge-converge.timer"),
        "and their timers were enabled before the CLI verdict reddened the run: {out}"
    );
    let c = &f.case;
    assert_eq!(c.summary("cli_sha"), SHA_A);
    assert!(
        c.summary("cli_result").starts_with("refused"),
        "{}",
        c.summary("cli_result")
    );
    assert_eq!(c.summary("cli_exit"), "1", "{out}");
    assert!(
        out.contains("DISTINCTIVE-CONNECTION-REFUSED") && out.contains("the CLI step FAILED"),
        "{out}"
    );
}

/// A hand run of install.sh (no converge around it) has no sha to
/// install for; it says so on the packet and installs no CLI rather
/// than guessing one.
#[test]
fn a_hand_run_without_a_converged_sha_installs_no_cli_and_says_so() {
    if !tools() {
        return;
    }
    let f = ForgeInstall::new("no-sha");
    let (rc, out) = f.run(FORGE_ROLES, None, &[]);
    assert_eq!(rc, 0, "{out}");
    assert!(f.case.requests().is_empty(), "{:?}", f.case.requests());
    assert!(
        f.case.summary("cli_result").starts_with("skipped"),
        "{}",
        f.case.summary("cli_result")
    );
    assert!(out.contains("BOSS_CONVERGE_SHA"), "the fix is named: {out}");
}

/// forge-converge.sh hands install.sh the sha it converged to under the
/// name install.sh reads — the loop itself needs root and a checkout
/// owner (runuser), so the handoff is pinned by text.
#[test]
fn the_forge_converge_hands_the_installer_the_sha_it_converged_to() {
    let converge = std::fs::read_to_string(repo_root().join(FORGE_CONVERGE)).unwrap();
    let install = std::fs::read_to_string(repo_root().join(FORGE_INSTALL)).unwrap();
    assert!(
        converge.contains("export BOSS_CONVERGE_SHA"),
        "{FORGE_CONVERGE}: exports BOSS_CONVERGE_SHA for install.sh"
    );
    assert!(
        install.contains("${BOSS_CONVERGE_SHA:-}")
            && install.contains("estate/install-cli-from-image.sh"),
        "{FORGE_INSTALL}: reads BOSS_CONVERGE_SHA and runs the estate installer"
    );
}
