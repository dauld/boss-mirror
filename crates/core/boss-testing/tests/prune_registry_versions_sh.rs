//! `infra/forge/prune-registry-versions.sh` is RUN, not read — against a
//! stubbed `curl` (the Forgejo packages API, the `/v2` manifest reads,
//! the system of record's train listing) and a stubbed `kubectl` (the
//! cluster's images), both recording every call — so every verdict
//! below is one the script actually reached. Nothing here touches the
//! forge, the registry, or a cluster.
//!
//! WHY THE VERB EXISTS (backlog 9789a827, David 2026-09-17). Measured
//! 2026-09-16: /opt/forgejo/data is 93 GB of the forge's 228 GB disk —
//! the container registry every train pushes a `boss:<sha>` image to
//! (~1–3 GB each, plus `boss-ci:<sha>` per train) — and nothing prunes
//! the REGISTRY side: prune-registry-tags.lib.sh removes LOCAL docker
//! tags only after the registry holds them (the registry IS the rollback
//! path), and disk-floor-sweep stops at the floor. The replacement disk
//! was not delivered, so the registry grows ~2 GB per train. The verb
//! deletes registry versions older than the last N landed trains,
//! keeping the live image, the rollback target, `latest`, anything under
//! 24 h, and everything it cannot classify.
//!
//! What each case pins:
//!
//!   * THE CREDENTIAL NEVER APPEARS IN ANY OUTPUT — the token, its
//!     base64 `auth` form, and the bearer minted from it — on any path.
//!     It rides in a curl config file, never in argv, and the stub
//!     verifies the header is exactly the docker login's.
//!   * THE KEEP SET IS DERIVED, AND A HALF THAT CANNOT BE DERIVED IS A
//!     REFUSAL: an unreadable cluster, a missing stamp, no landed shas,
//!     a docker config with no auth for the registry — each refuses
//!     naming the half, and nothing is deleted.
//!   * A 403 NAMES THE SCOPE the forge asked for — `read:package` on
//!     the listing, `write:package` on the first DELETE — and the real
//!     run stops there, before a second attempt.
//!   * THE DRY RUN SENDS NO DELETE and prints the same record line the
//!     real run would, flagged `dry_run: true`.
//!   * THE REAL RUN DELETES EXACTLY THE PLANNED SET — tags first, then
//!     the untagged child manifests only those tags referenced, then
//!     orphans no readable index references — and never a child a kept
//!     tag shares.
//!   * A TAG WHOSE INDEX CANNOT BE READ IS NOT DELETED, and while any
//!     index is unreadable no orphan is either: unclassified is kept.
//!   * THE VERB FILE serves the forge, is MUTATING, names David and the
//!     packet, takes mode plus an optional keep count, and declares a
//!     timeout for ~1,400 versions of reads.
//!   * THE VERDICT COMES FIRST (backlog 5323f3ef, measured 2026-09-17
//!     05:27Z on the first dry run, ops-request 279b8659): the verb
//!     printed 1,313 per-version lines (165 KB) and the JSON record
//!     LAST; the ops runner keeps the first 100 KB, so the packet held
//!     the plan's head and none of its verdict. Now the record and the
//!     summary precede the per-version list in the runner's combined
//!     capture, and the full list is a file on the forge the record
//!     names — refused before any DELETE when it cannot be written.

use boss_testing::{repo_root, scratch_dir, write_exec, write_file};
use std::path::PathBuf;
use std::process::Command;

const SCRIPT: &str = "infra/forge/prune-registry-versions.sh";
const TOKEN: &str = "forgetok0123456789abcdefFORGE";
/// base64("david:forgetok0123456789abcdefFORGE") — what `docker login`
/// writes under `auths."10.20.0.15:3000".auth`.
const AUTH_B64: &str = "ZGF2aWQ6Zm9yZ2V0b2swMTIzNDU2Nzg5YWJjZGVmRk9SR0U=";
const BEARER: &str = "stub-bearer-jwt-9f8e7d6c";
const REGISTRY: &str = "10.20.0.15:3000/david/boss";

fn has(tool: &str) -> bool {
    Command::new("sh")
        .args(["-c", &format!("command -v {tool} >/dev/null 2>&1")])
        .status()
        .is_ok_and(|s| s.success())
}

/// An RFC 3339 stamp `hours` ago, the way GNU date prints it — the
/// script parses `created_at` with the same tool.
fn hours_ago(hours: u64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let at = now - hours * 3600;
    let out = Command::new("date")
        .args(["-u", "-d", &format!("@{at}"), "+%Y-%m-%dT%H:%M:%SZ"])
        .output()
        .expect("date runs");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn version(id: u64, name: &str, version: &str, created_at: &str) -> serde_json::Value {
    serde_json::json!({
        "id": id,
        "owner": {"login": "david"},
        "type": "container",
        "name": name,
        "version": version,
        "created_at": created_at,
    })
}

/// A manifest index (what `docker push` puts under a tag when buildx
/// attaches provenance): the children are untagged `sha256:` versions
/// in the registry, and they are where the layer files live.
fn index(children: &[&str]) -> String {
    serde_json::json!({
        "schemaVersion": 2,
        "mediaType": "application/vnd.oci.image.index.v1+json",
        "manifests": children.iter().map(|c| serde_json::json!({
            "mediaType": "application/vnd.oci.image.manifest.v1+json",
            "digest": format!("sha256:{c}"),
            "size": 1234,
        })).collect::<Vec<_>>(),
    })
    .to_string()
}

fn digest(name: &str) -> String {
    // 64 hex characters, distinct per name, so the fixture reads like
    // the registry does.
    let mut s = String::new();
    for b in name.bytes().cycle().take(32) {
        s.push_str(&format!("{b:02x}"));
    }
    format!("sha256:{s}")
}

/// The registry as the fixture declares it, all on the versions the
/// keep set must judge. Ages: fresh = 2 h, old = 5 days.
///
///   boss:
///     bdf435d  live (deploy boss in ns boss)          keep: live
///     aaaaaaa  live (deploy boss in ns boss-playground) keep: live
///     b2814ef  the stamp                              keep: stamp
///     1111111  landed train                           keep: landed
///     2222222  landed train                           keep: landed
///     f0e5401  fresh (2 h)                            keep: fresh
///     latest                                          keep: latest
///     v1       not a sha                              unclassified
///     01d0001  old, no keep reason                    DELETE (tag)
///     deadbee  old, no keep reason                    DELETE (tag)
///     children: live1, live2 (of bdf435d); shared (of bdf435d AND
///     deadbee → kept); old1, old2 (of 01d0001 → DELETE); dead1 (of
///     deadbee → DELETE); stamp1 (of b2814ef → kept); orphan-old (no
///     index → DELETE); orphan-fresh (no index, 2 h → kept)
///   boss-ci (page 2):
///     1111111…(40)  landed → keep;  01d0001…(40) old → DELETE with
///     child ci-old1;  rust1.96 → unclassified, child ci-base kept
///   boss-ci-cache: never a candidate (not one of the two names)
struct Case {
    bin: PathBuf,
    stub: PathBuf,
    curl_log: PathBuf,
    deletes: PathBuf,
    kubectl_log: PathBuf,
    docker_config: PathBuf,
    stamp: PathBuf,
    df_path: PathBuf,
    list_dir: PathBuf,
}

fn forty(seven: &str) -> String {
    let mut s = seven.to_string();
    while s.len() < 40 {
        s.push_str(seven);
    }
    s.truncate(40);
    s
}

impl Case {
    fn new(name: &str) -> Self {
        let root = scratch_dir(&format!("prune-registry-versions-{name}"));
        let bin = root.join("bin");
        let stub = root.join("stub");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(&stub).unwrap();
        let curl_log = root.join("curl.log");
        let deletes = root.join("deletes.log");
        let kubectl_log = root.join("kubectl.log");
        let docker_config = root.join("docker-config.json");
        let stamp = root.join("boss-last-built");
        let df_path = root.join("forgejo-data");
        std::fs::create_dir_all(&df_path).unwrap();
        // Where the per-version list lands (the forge's
        // /var/backups/boss/registry-prune in production): not created
        // here — the script creates it.
        let list_dir = root.join("registry-prune");

        write_file(
            &docker_config,
            &format!(
                r#"{{"auths": {{"10.20.0.15:3000": {{"auth": "{AUTH_B64}"}}, "https://index.docker.io/v1/": {{"auth": "bm9ib2R5OmJvZ3Vz"}}}}}}"#
            ),
        );
        write_file(&stamp, "b2814ef\n");

        let fresh = hours_ago(2);
        let old = hours_ago(5 * 24);
        // One old stamp written the way Forgejo prints it (an offset,
        // not Z), so the parse is exercised on both shapes.
        let old_offset = "2026-09-01T00:00:00-07:00";
        let mut id = 3000u64;
        let mut next = || {
            id += 1;
            id
        };
        let page1 = serde_json::json!([
            version(next(), "boss", "bdf435d", &fresh),
            version(next(), "boss", &digest("live1"), &fresh),
            version(next(), "boss", &digest("live2"), &fresh),
            version(next(), "boss", &digest("shared"), &old),
            version(next(), "boss", "aaaaaaa", &old),
            version(next(), "boss", "b2814ef", &old),
            version(next(), "boss", &digest("stamp1"), &old),
            version(next(), "boss", "1111111", &old),
            version(next(), "boss", "2222222", old_offset),
            version(next(), "boss", "f0e5401", &fresh),
            version(next(), "boss", "latest", &old),
            version(next(), "boss", "v1", &old),
            version(next(), "boss", "01d0001", &old),
            version(next(), "boss", &digest("old1"), &old),
            version(next(), "boss", &digest("old2"), &old),
            version(next(), "boss", "deadbee", old_offset),
            version(next(), "boss", &digest("dead1"), &old),
            version(next(), "boss", &digest("orphan-old"), &old),
            version(next(), "boss", &digest("orphan-fresh"), &fresh),
            version(next(), "boss-ci-cache", "deadbee", &old),
        ]);
        let page2 = serde_json::json!([
            version(next(), "boss-ci", &forty("1111111"), &old),
            version(next(), "boss-ci", &forty("01d0001"), &old),
            version(next(), "boss-ci", &digest("ci-old1"), &old),
            version(next(), "boss-ci", "rust1.96", &old),
            version(next(), "boss-ci", &digest("ci-base"), &old),
        ]);
        write_file(&stub.join("page-1.json"), &page1.to_string());
        write_file(&stub.join("page-2.json"), &page2.to_string());
        write_file(&stub.join("empty.json"), "[]");
        write_file(&stub.join("total"), "25");
        write_file(
            &stub.join("token.json"),
            &format!(r#"{{"token": "{BEARER}"}}"#),
        );
        write_file(
            &stub.join("notfound.json"),
            r#"{"errors":[{"code":"MANIFEST_UNKNOWN","message":"manifest unknown"}]}"#,
        );
        write_file(
            &stub.join("forbidden-read.json"),
            r#"{"message":"token does not have at least one of required scope(s), required=[read:package]","url":"http://10.20.0.15:3000/api/swagger"}"#,
        );
        write_file(
            &stub.join("forbidden-write.json"),
            r#"{"message":"token does not have at least one of required scope(s), required=[write:package]","url":"http://10.20.0.15:3000/api/swagger"}"#,
        );
        let idx = |tag: &str, kids: &[&str]| {
            write_file(&stub.join(format!("index-boss-{tag}.json")), &index(kids));
        };
        let d = |n: &str| digest(n).trim_start_matches("sha256:").to_string();
        idx("bdf435d", &[&d("live1"), &d("live2"), &d("shared")]);
        idx("aaaaaaa", &[]);
        idx("b2814ef", &[&d("stamp1")]);
        idx("1111111", &[]);
        idx("2222222", &[]);
        idx("f0e5401", &[]);
        idx("latest", &[]);
        idx("v1", &[]);
        idx("01d0001", &[&d("old1"), &d("old2")]);
        idx("deadbee", &[&d("dead1"), &d("shared")]);
        write_file(
            &stub.join(format!("index-boss-ci-{}.json", forty("1111111"))),
            &index(&[]),
        );
        write_file(
            &stub.join(format!("index-boss-ci-{}.json", forty("01d0001"))),
            &index(&[&d("ci-old1")]),
        );
        write_file(
            &stub.join("index-boss-ci-rust1.96.json"),
            &index(&[&d("ci-base")]),
        );

        // The system of record: two closed trains (1111111 landed via
        // train_ref, 2222222 via merge_ref) and one open train that
        // references 3333333.
        write_file(
            &stub.join("trains.json"),
            &serde_json::json!({
                "total": 3,
                "data": [
                    {"status": "open", "steps": [{"metadata": {"train_ref": "train/2026-09-17-0100@3333333"}}]},
                    {"status": "closed", "steps": [{"metadata": {"train_ref": "train/2026-09-17-0000@1111111"}}, {"metadata": {"merge_ref": "222222222222"}}]},
                    {"status": "closed", "steps": [{"metadata": {"train_ref": "train/2026-09-16-2300@1111111"}}]},
                ]
            })
            .to_string(),
        );

        // The cluster's images, as `kubectl get deploy,sts,cronjob -A
        // -o json` lists them: the prod deploy on bdf435d (main and
        // init), a playground instance on aaaaaaa, chores on latest,
        // the dev pod on boss-ci:rust1.96.
        write_file(
            &stub.join("cluster.json"),
            &serde_json::json!({
                "kind": "List",
                "items": [
                    {"kind": "Deployment", "metadata": {"name": "boss", "namespace": "boss"},
                     "spec": {"template": {"spec": {
                        "initContainers": [{"name": "boss-init", "image": format!("{REGISTRY}:bdf435d")}],
                        "containers": [{"name": "boss", "image": format!("{REGISTRY}:bdf435d")}]}}}},
                    {"kind": "Deployment", "metadata": {"name": "boss", "namespace": "boss-playground"},
                     "spec": {"template": {"spec": {
                        "containers": [{"name": "boss", "image": format!("{REGISTRY}:aaaaaaa")}]}}}},
                    {"kind": "StatefulSet", "metadata": {"name": "postgres", "namespace": "boss"},
                     "spec": {"template": {"spec": {
                        "containers": [{"name": "postgres", "image": "10.20.0.15:3000/david/postgres:16"}]}}}},
                    {"kind": "CronJob", "metadata": {"name": "boss-files-gc", "namespace": "boss"},
                     "spec": {"jobTemplate": {"spec": {"template": {"spec": {
                        "containers": [{"name": "chore", "image": format!("{REGISTRY}:latest")}]}}}}}},
                    {"kind": "Deployment", "metadata": {"name": "boss-dev", "namespace": "boss-dev"},
                     "spec": {"template": {"spec": {
                        "containers": [{"name": "dev", "image": "10.20.0.15:3000/david/boss-ci:rust1.96"}]}}}},
                ]
            })
            .to_string(),
        );

        // The stub curl. Every call is logged as `METHOD URL auth=<kind>`
        // where <kind> says which credential the -K config carried:
        // `basic-ok` (the docker login's exact header), `bearer-ok` (the
        // minted token), `none`, or `other`. Answers come from $STUB_DIR
        // by URL shape; a DELETE is appended to $STUB_DELETES and answers
        // 204 unless `delete-code` (with `delete-body`) or
        // `delete-fail-on` (a version) says otherwise. `-f` fails on a
        // 4xx with exit 22, the way curl does (landed_train_shas uses
        // -fsS).
        write_exec(
            &bin.join("curl"),
            r#"#!/usr/bin/env bash
set -u
method=GET; out=""; want_code=0; url=""; cfg=""; hdr=""; fail=0
args=("$@"); i=0
while [ $i -lt ${#args[@]} ]; do
    a="${args[$i]}"
    case "$a" in
        -o) i=$((i+1)); out="${args[$i]}" ;;
        -w) i=$((i+1)); want_code=1 ;;
        -X) i=$((i+1)); method="${args[$i]}" ;;
        -H) i=$((i+1)) ;;
        -K) i=$((i+1)); cfg="${args[$i]}" ;;
        -D) i=$((i+1)); hdr="${args[$i]}" ;;
        --max-time) i=$((i+1)) ;;
        --*) ;;
        -*) case "$a" in *f*) fail=1 ;; esac ;;
        *) url="$a" ;;
    esac
    i=$((i+1))
done
auth=none
if [ -n "$cfg" ] && [ -f "$cfg" ]; then
    if grep -qxF "header = \"Authorization: Basic $STUB_AUTH_B64\"" "$cfg"; then auth=basic-ok
    elif grep -qxF "header = \"Authorization: Bearer $STUB_BEARER\"" "$cfg"; then auth=bearer-ok
    elif grep -q 'Authorization' "$cfg"; then auth=other
    fi
fi
printf '%s %s auth=%s\n' "$method" "$url" "$auth" >> "$STUB_CURL_LOG"
emit() { # <code> <body-file>
    local code="$1" body="$2"
    if [ -n "$hdr" ]; then printf 'HTTP/1.1 %s\r\nX-Total-Count: %s\r\n\r\n' "$code" "$(cat "$STUB_DIR/total")" > "$hdr"; fi
    if [ -n "$out" ]; then cat "$body" > "$out"; else cat "$body"; fi
    [ "$want_code" = 1 ] && printf '%s' "$code"
    if [ "$fail" = 1 ] && [ "$code" -ge 400 ]; then exit 22; fi
    exit 0
}
case "$url" in
    */api/jobs?kind=pr-train*)
        [ -f "$STUB_DIR/trains-down" ] && { echo 'curl: (7) Failed to connect' >&2; exit 7; }
        emit 200 "$STUB_DIR/trains.json" ;;
    */api/v1/packages/david?type=container*)
        [ "$auth" = basic-ok ] || emit 401 "$STUB_DIR/notfound.json"
        if [ -f "$STUB_DIR/list-code" ]; then emit "$(cat "$STUB_DIR/list-code")" "$STUB_DIR/list-body"; fi
        page=$(printf '%s' "$url" | sed -n 's/.*[?&]page=\([0-9]*\).*/\1/p')
        f="$STUB_DIR/page-$page.json"; [ -f "$f" ] || f="$STUB_DIR/empty.json"
        emit 200 "$f" ;;
    */v2/token*)
        [ "$auth" = basic-ok ] || emit 401 "$STUB_DIR/notfound.json"
        [ -f "$STUB_DIR/no-token" ] && emit 401 "$STUB_DIR/notfound.json"
        emit 200 "$STUB_DIR/token.json" ;;
    */v2/david/*/manifests/*)
        [ "$auth" = bearer-ok ] || emit 401 "$STUB_DIR/notfound.json"
        rest="${url#*/v2/david/}"; name="${rest%%/manifests/*}"; tag="${rest##*/manifests/}"
        f="$STUB_DIR/index-$name-$tag.json"
        [ -f "$f" ] && emit 200 "$f"
        emit 404 "$STUB_DIR/notfound.json" ;;
    */api/v1/packages/david/container/*)
        [ "$method" = DELETE ] || { echo "stub curl: unexpected $method on $url" >&2; exit 1; }
        [ "$auth" = basic-ok ] || emit 401 "$STUB_DIR/notfound.json"
        rest="${url#*/container/}"; name="${rest%%/*}"; ver="${rest#*/}"
        printf '%s %s\n' "$name" "$ver" >> "$STUB_DELETES"
        if [ -f "$STUB_DIR/delete-code" ]; then emit "$(cat "$STUB_DIR/delete-code")" "$STUB_DIR/delete-body"; fi
        if [ -f "$STUB_DIR/delete-fail-on" ] && [ "$ver" = "$(cat "$STUB_DIR/delete-fail-on")" ]; then emit 500 "$STUB_DIR/notfound.json"; fi
        emit 204 /dev/null ;;
esac
echo "stub curl: unexpected url $url" >&2
exit 1
"#,
        );
        // The stub kubectl: logs argv; answers the one read the script
        // makes with the cluster fixture, or fails on STUB_KUBECTL_FAIL.
        write_exec(
            &bin.join("kubectl"),
            r#"#!/usr/bin/env bash
set -u
{ printf '%s\n' "$@"; echo '=== call ==='; } >> "$STUB_KUBECTL_LOG"
[ -n "${STUB_KUBECTL_FAIL:-}" ] && { echo 'error: You must be logged in to the server (Unauthorized)' >&2; exit 1; }
case " $* " in
    *" get deploy,sts,cronjob -A -o json "*) cat "$STUB_DIR/cluster.json"; exit 0 ;;
esac
echo "stub kubectl: unexpected argv: $*" >&2
exit 1
"#,
        );
        Case {
            bin,
            stub,
            curl_log,
            deletes,
            kubectl_log,
            docker_config,
            stamp,
            df_path,
            list_dir,
        }
    }

    fn run(&self, args: &[&str]) -> (i32, String) {
        self.run_env(args, &[])
    }

    fn run_env(&self, args: &[&str], extra: &[(&str, String)]) -> (i32, String) {
        let mut cmd = Command::new("bash");
        cmd.arg(repo_root().join(SCRIPT)).args(args);
        self.env(&mut cmd, extra);
        let out = cmd.output().expect("prune-registry-versions.sh runs");
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        (out.status.code().unwrap_or(-1), text)
    }

    /// The streams interleaved the way the ops runner captures them
    /// (`> raw 2>&1`, ops-runner.sh) — the only reading in which the
    /// ORDER of the record against the per-version lines means
    /// anything, since the runner keeps the first 100 KB of that file.
    fn run_combined(&self, args: &[&str]) -> (i32, String) {
        let mut cmd = Command::new("bash");
        cmd.arg("-c")
            .arg(r#"exec bash "$@" 2>&1"#)
            .arg("_")
            .arg(repo_root().join(SCRIPT))
            .args(args);
        self.env(&mut cmd, &[]);
        let out = cmd.output().expect("prune-registry-versions.sh runs");
        assert!(out.stderr.is_empty(), "stderr was not merged");
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
        )
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
            .env("BOSS_KUBECTL", self.bin.join("kubectl"))
            .env("BOSS_JOBS_URL", "http://sor.invalid:7900")
            .env("BOSS_FORGE_REGISTRY", REGISTRY)
            .env("BOSS_PRUNE_DOCKER_CONFIG", &self.docker_config)
            .env("BOSS_FORGE_LAST_BUILT", &self.stamp)
            .env("BOSS_PRUNE_DF_PATH", &self.df_path)
            .env("BOSS_PRUNE_LIST_DIR", &self.list_dir)
            .env("STUB_DIR", &self.stub)
            .env("STUB_CURL_LOG", &self.curl_log)
            .env("STUB_DELETES", &self.deletes)
            .env("STUB_KUBECTL_LOG", &self.kubectl_log)
            .env("STUB_AUTH_B64", AUTH_B64)
            .env("STUB_BEARER", BEARER);
        for (k, v) in extra {
            cmd.env(k, v);
        }
    }

    /// The DELETEs the registry saw, in order, as `name version`.
    fn deleted(&self) -> Vec<String> {
        std::fs::read_to_string(&self.deletes)
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn curl_calls(&self) -> Vec<String> {
        std::fs::read_to_string(&self.curl_log)
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    /// The one JSON record line on stdout — the line that names the
    /// verb (a failure also echoes the forge's JSON body, on stderr).
    /// Found by content, not position; in the runner's combined capture
    /// it is the FIRST JSON line, and
    /// `the_verdict_precedes_the_per_version_list_and_the_list_is_a_named_file`
    /// pins that.
    fn record(&self, text: &str) -> serde_json::Value {
        text.lines()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .find(|v| v["verb"] == "prune-registry-versions")
            .unwrap_or_else(|| panic!("no JSON record line in:\n{text}"))
    }
}

fn contains_all(text: &str, needles: &[&str], what: &str) {
    for n in needles {
        assert!(text.contains(n), "{what}: expected `{n}` in:\n{text}");
    }
}

fn no_credential(text: &str, what: &str) {
    for secret in [TOKEN, AUTH_B64, BEARER, "forgetok"] {
        assert!(
            !text.contains(secret),
            "{what}: the credential ({secret}) reached the output:\n{text}"
        );
    }
}

// ---------------------------------------------------------------------------
// The mode and the arguments.
// ---------------------------------------------------------------------------

#[test]
fn refuses_a_missing_or_foreign_mode_or_keep_count() {
    if !has("jq") {
        return;
    }
    let c = Case::new("args");
    for (args, why) in [
        (vec![], "no mode"),
        (vec!["--yes"], "a foreign mode"),
        (vec!["--dry-run", "0"], "a zero keep count"),
        (vec!["--dry-run", "ten"], "a non-numeric keep count"),
        (vec!["--dry-run", "10", "extra"], "a fourth word"),
    ] {
        let (rc, out) = c.run(&args);
        assert_eq!(rc, 2, "{why}: {out}");
        assert!(c.deleted().is_empty(), "{why}: something was deleted");
        no_credential(&out, why);
    }
    let (rc, out) = c.run(&["--dry-run", "5"]);
    assert_eq!(rc, 0, "a keep count of 5 is a valid argument: {out}");
}

#[test]
fn refuses_without_a_system_of_record_or_a_registry_credential() {
    if !has("jq") {
        return;
    }
    let c = Case::new("no-sor");
    let (rc, out) = c.run_env(&["--dry-run"], &[("BOSS_JOBS_URL", String::new())]);
    assert_eq!(rc, 2, "{out}");
    contains_all(&out, &["BOSS_JOBS_URL", "Nothing was deleted"], "no SoR");

    // No auth for the registry host in the docker config: the refusal
    // names the file and the registry key, never a value.
    let c = Case::new("no-auth");
    write_file(
        &c.docker_config,
        r#"{"auths": {"https://index.docker.io/v1/": {"auth": "bm9ib2R5OmJvZ3Vz"}}}"#,
    );
    let (rc, out) = c.run(&["--dry-run"]);
    assert_eq!(rc, 2, "{out}");
    contains_all(
        &out,
        &[
            "10.20.0.15:3000",
            "docker-config.json",
            "docker login",
            "Nothing was deleted",
        ],
        "no auth for the registry",
    );
    assert!(
        !out.contains("bm9ib2R5"),
        "another registry's auth leaked: {out}"
    );

    // A credential store instead of an inline auth: refused, named.
    let c = Case::new("credstore");
    write_file(
        &c.docker_config,
        r#"{"auths": {"10.20.0.15:3000": {}}, "credsStore": "pass"}"#,
    );
    let (rc, out) = c.run(&["--dry-run"]);
    assert_eq!(rc, 2, "{out}");
    contains_all(&out, &["credsStore", "Nothing was deleted"], "credsStore");

    // An absent file.
    let c = Case::new("no-config");
    std::fs::remove_file(&c.docker_config).unwrap();
    let (rc, out) = c.run(&["--dry-run"]);
    assert_eq!(rc, 2, "{out}");
    contains_all(
        &out,
        &["docker-config.json", "Nothing was deleted"],
        "no config",
    );
    assert!(c.curl_calls().is_empty(), "no credential, no call: {out}");
}

// ---------------------------------------------------------------------------
// The keep set: every half that cannot be derived is a refusal.
// ---------------------------------------------------------------------------

#[test]
fn refuses_when_the_cluster_the_stamp_or_the_landed_trains_cannot_be_read() {
    if !has("jq") {
        return;
    }
    let c = Case::new("no-cluster");
    let (rc, out) = c.run_env(&["--dry-run"], &[("STUB_KUBECTL_FAIL", "1".into())]);
    assert_eq!(rc, 2, "{out}");
    contains_all(
        &out,
        &["live", "kubectl", "Nothing was deleted"],
        "kubectl fails",
    );
    assert!(c.deleted().is_empty());

    // A cluster with no deploy boss in ns boss: the named live image
    // is not there, so the keep set is not derivable.
    let c = Case::new("no-live");
    write_file(
        &c.stub.join("cluster.json"),
        r#"{"kind":"List","items":[]}"#,
    );
    let (rc, out) = c.run(&["--dry-run"]);
    assert_eq!(rc, 2, "{out}");
    contains_all(
        &out,
        &["deploy", "boss", "Nothing was deleted"],
        "no live image",
    );

    let c = Case::new("no-stamp");
    std::fs::remove_file(&c.stamp).unwrap();
    let (rc, out) = c.run(&["--dry-run"]);
    assert_eq!(rc, 2, "{out}");
    contains_all(
        &out,
        &["boss-last-built", "rollback", "Nothing was deleted"],
        "no stamp",
    );

    let c = Case::new("bad-stamp");
    write_file(&c.stamp, "not a sha\n");
    let (rc, out) = c.run(&["--dry-run"]);
    assert_eq!(rc, 2, "{out}");
    contains_all(
        &out,
        &["boss-last-built", "Nothing was deleted"],
        "bad stamp",
    );

    // No landed trains: the SoR listed none (a failed read, not a fact).
    let c = Case::new("no-landed");
    write_file(&c.stub.join("trains.json"), r#"{"total": 0, "data": []}"#);
    let (rc, out) = c.run(&["--dry-run"]);
    assert_eq!(rc, 2, "{out}");
    contains_all(&out, &["landed", "Nothing was deleted"], "no landed trains");

    let c = Case::new("sor-down");
    write_file(&c.stub.join("trains-down"), "");
    let (rc, out) = c.run(&["--dry-run"]);
    assert_eq!(rc, 2, "{out}");
    contains_all(&out, &["landed", "Nothing was deleted"], "SoR down");
    // The keep set is derived BEFORE the registry is read: no listing,
    // no manifest read, and nothing deleted.
    assert!(
        !c.curl_calls()
            .iter()
            .any(|l| l.contains("/api/v1/packages/")),
        "the registry was read before the keep set existed: {:?}",
        c.curl_calls()
    );
}

#[test]
fn a_403_on_the_listing_names_the_scope_and_deletes_nothing() {
    if !has("jq") {
        return;
    }
    let c = Case::new("forbidden-read");
    write_file(&c.stub.join("list-code"), "403");
    std::fs::copy(c.stub.join("forbidden-read.json"), c.stub.join("list-body")).unwrap();
    for mode in ["--dry-run", "--for-real"] {
        let (rc, out) = c.run(&[mode]);
        assert_eq!(rc, 2, "{mode}: {out}");
        contains_all(
            &out,
            &["read:package", "403", "David", "Nothing was deleted"],
            mode,
        );
        no_credential(&out, mode);
        assert!(c.deleted().is_empty(), "{mode} deleted something");
    }
}

// ---------------------------------------------------------------------------
// The dry run.
// ---------------------------------------------------------------------------

#[test]
fn dry_run_plans_the_set_sends_no_delete_and_records() {
    if !has("jq") {
        return;
    }
    let c = Case::new("dry-run");
    let (rc, out) = c.run(&["--dry-run"]);
    assert_eq!(rc, 0, "{out}");
    no_credential(&out, "dry run");
    assert!(
        c.deleted().is_empty(),
        "the dry run deleted: {:?}",
        c.deleted()
    );
    assert!(
        !c.curl_calls().iter().any(|l| l.starts_with("DELETE ")),
        "the dry run sent a DELETE: {:?}",
        c.curl_calls()
    );
    // The keep set is stated, half by half.
    contains_all(
        &out,
        &[
            "live: bdf435d",
            "aaaaaaa",
            "stamp: b2814ef",
            "landed: 2",
            "latest",
            "24",
        ],
        "keep set",
    );
    // The plan names every deletion and every unclassified version.
    contains_all(
        &out,
        &[
            "would DELETE boss:01d0001",
            "would DELETE boss:deadbee",
            &format!("would DELETE boss:{}", digest("old1")),
            &format!("would DELETE boss:{}", digest("dead1")),
            &format!("would DELETE boss:{}", digest("orphan-old")),
            &format!("would DELETE boss-ci:{}", forty("01d0001")),
            &format!("would DELETE boss-ci:{}", digest("ci-old1")),
            "unclassified boss:v1",
            "unclassified boss-ci:rust1.96",
            "DRY RUN",
            "Nothing was deleted",
        ],
        "plan",
    );
    for kept in [
        "boss:bdf435d",
        "boss:b2814ef",
        "boss:1111111",
        "boss:2222222",
        "boss:f0e5401",
        "boss:latest",
        &format!("boss:{}", digest("shared")),
        &format!("boss:{}", digest("stamp1")),
        &format!("boss:{}", digest("orphan-fresh")),
        &format!("boss-ci:{}", forty("1111111")),
        &format!("boss-ci:{}", digest("ci-base")),
    ] {
        assert!(
            !out.contains(&format!("would DELETE {kept}")),
            "{kept} is in the keep set and was planned for deletion:\n{out}"
        );
    }
    let r = c.record(&out);
    assert_eq!(r["verb"], "prune-registry-versions");
    assert_eq!(r["dry_run"], true);
    assert_eq!(r["keep"]["stamp"], "b2814ef");
    assert_eq!(r["keep"]["landed_trains"], 2);
    assert_eq!(r["keep"]["keep_trains"], 10);
    assert_eq!(r["keep"]["newer_than_hours"], 24);
    let live = r["keep"]["live"].as_array().unwrap();
    assert!(live.iter().any(|v| v == "boss:bdf435d"), "{live:?}");
    assert!(live.iter().any(|v| v == "boss:aaaaaaa"), "{live:?}");
    let boss = &r["packages"]["boss"];
    assert_eq!(boss["versions"], 19);
    assert_eq!(boss["delete"], 6, "{boss}");
    assert_eq!(boss["unclassified"], 1, "{boss}");
    assert_eq!(boss["kept"], 12, "{boss}");
    assert_eq!(boss["deleted"], 0);
    let ci = &r["packages"]["boss-ci"];
    assert_eq!(ci["versions"], 5);
    assert_eq!(ci["delete"], 2, "{ci}");
    assert_eq!(ci["unclassified"], 1, "{ci}");
    assert!(r["packages"].get("boss-ci-cache").is_none(), "{r}");
    assert_eq!(r["listed"], 25);
    assert_eq!(r["declared_bytes"], serde_json::Value::Null);
    assert!(r["disk_avail_kb_before"].is_number(), "{r}");
    // The scope the real run needs is stated as unmeasured — a dry run
    // cannot prove write:package without deleting.
    assert!(
        r["write_scope"].as_str().unwrap().contains("unmeasured"),
        "{r}"
    );
}

#[test]
fn the_credential_rides_in_a_config_file_not_argv() {
    if !has("jq") {
        return;
    }
    let c = Case::new("cred-in-file");
    let (rc, out) = c.run(&["--dry-run"]);
    assert_eq!(rc, 0, "{out}");
    let calls = c.curl_calls();
    assert!(
        calls
            .iter()
            .filter(|l| l.contains("/api/v1/packages/david?type=container"))
            .all(|l| l.ends_with("auth=basic-ok")),
        "the listing did not carry the docker login's exact header: {calls:?}"
    );
    assert!(
        calls
            .iter()
            .filter(|l| l.contains("/manifests/"))
            .all(|l| l.ends_with("auth=bearer-ok")),
        "the manifest reads did not carry the minted bearer: {calls:?}"
    );
    assert!(
        calls
            .iter()
            .any(|l| l.contains("/v2/token") && l.ends_with("auth=basic-ok")),
        "the bearer was not minted through /v2/token with the login: {calls:?}"
    );
    // Every tagged version's index was read, kept or not: a kept tag's
    // children must be known so a shared child is never deleted.
    for tag in ["bdf435d", "deadbee", "latest", "v1"] {
        assert!(
            calls
                .iter()
                .any(|l| l.contains(&format!("/v2/david/boss/manifests/{tag} "))),
            "no index read for {tag}: {calls:?}"
        );
    }
    // A limit is not a filter: the listing walked past the first page.
    assert!(calls.iter().any(|l| l.contains("page=2")), "{calls:?}");
    assert!(calls.iter().any(|l| l.contains("page=3")), "{calls:?}");
}

// ---------------------------------------------------------------------------
// The real run.
// ---------------------------------------------------------------------------

#[test]
fn the_real_run_deletes_exactly_the_planned_set_tags_first_and_records() {
    if !has("jq") {
        return;
    }
    let c = Case::new("for-real");
    let (rc, out) = c.run(&["--for-real"]);
    assert_eq!(rc, 0, "{out}");
    no_credential(&out, "real run");
    let deleted = c.deleted();
    let mut expect: Vec<String> = vec![
        "boss 01d0001".into(),
        "boss deadbee".into(),
        format!("boss-ci {}", forty("01d0001")),
        format!("boss {}", digest("old1")),
        format!("boss {}", digest("old2")),
        format!("boss {}", digest("dead1")),
        format!("boss {}", digest("orphan-old")),
        format!("boss-ci {}", digest("ci-old1")),
    ];
    let mut got = deleted.clone();
    // Tags before children: the first three are the tags, in listing
    // order; the children follow.
    assert_eq!(
        &deleted[..3],
        &[
            "boss 01d0001".to_string(),
            "boss deadbee".to_string(),
            format!("boss-ci {}", forty("01d0001"))
        ],
        "tags first: {deleted:?}"
    );
    expect.sort();
    got.sort();
    assert_eq!(got, expect, "the deleted set is not the plan:\n{out}");
    let r = c.record(&out);
    assert_eq!(r["dry_run"], false);
    assert_eq!(r["packages"]["boss"]["deleted"], 6, "{r}");
    assert_eq!(r["packages"]["boss-ci"]["deleted"], 2, "{r}");
    assert_eq!(r["packages"]["boss"]["kept"], 12, "{r}");
    assert_eq!(r["write_scope"], "write:package proven by DELETE", "{r}");
    assert!(r["disk_avail_kb_after"].is_number(), "{r}");
    contains_all(
        &out,
        &["OK", "deleted 8", "df", "Forgejo"],
        "the closing line says what happened and what the bytes wait on",
    );
}

#[test]
fn a_403_on_the_first_delete_refuses_naming_the_scope_and_stops() {
    if !has("jq") {
        return;
    }
    let c = Case::new("forbidden-write");
    write_file(&c.stub.join("delete-code"), "403");
    std::fs::copy(
        c.stub.join("forbidden-write.json"),
        c.stub.join("delete-body"),
    )
    .unwrap();
    let (rc, out) = c.run(&["--for-real"]);
    assert_eq!(rc, 2, "{out}");
    contains_all(
        &out,
        &["write:package", "403", "David", "Nothing was deleted"],
        "403 on delete",
    );
    no_credential(&out, "403 on delete");
    assert_eq!(
        c.deleted().len(),
        1,
        "a second DELETE was attempted after the refusal: {:?}",
        c.deleted()
    );
    let r = c.record(&out);
    assert_eq!(r["packages"]["boss"]["deleted"], 0, "{r}");
    assert_eq!(r["refused"], "write:package", "{r}");
}

#[test]
fn a_failed_delete_mid_way_stops_and_states_what_was_deleted() {
    if !has("jq") {
        return;
    }
    let c = Case::new("mid-fail");
    write_file(&c.stub.join("delete-fail-on"), "deadbee");
    let (rc, out) = c.run(&["--for-real"]);
    assert_eq!(rc, 1, "{out}");
    assert_eq!(c.deleted().len(), 2, "{:?}", c.deleted());
    contains_all(
        &out,
        &["FAILED", "deadbee", "500", "deleted 1"],
        "mid-way failure",
    );
    let r = c.record(&out);
    assert_eq!(r["packages"]["boss"]["deleted"], 1, "{r}");
    assert_eq!(r["packages"]["boss"]["failed"], 1, "{r}");
}

// ---------------------------------------------------------------------------
// The verdict comes first, and the full list is a file.
// ---------------------------------------------------------------------------

/// Backlog 5323f3ef (measured 2026-09-17 05:27Z, ops-request 279b8659):
/// 1,313 `would DELETE` lines (165 KB) and the record printed last; the
/// runner kept the first 100 KB, and the packet held no verdict. The
/// order is judged on the streams merged the way the runner merges
/// them, and the record must sit inside the first 4 KB — the window
/// the landing probe reads on the next ops-request.
#[test]
fn the_verdict_precedes_the_per_version_list_and_the_list_is_a_named_file() {
    if !has("jq") {
        return;
    }
    for (mode, summary) in [("--dry-run", "DRY RUN"), ("--for-real", "OK —")] {
        let c = Case::new(&format!("verdict-first{mode}"));
        let (rc, out) = c.run_combined(&[mode]);
        assert_eq!(rc, 0, "{mode}: {out}");
        let at = |needle: &str| {
            out.find(needle)
                .unwrap_or_else(|| panic!("{mode}: no `{needle}` in:\n{out}"))
        };
        let record_at = at(r#"{"verb":"prune-registry-versions""#);
        let first_delete = at("would DELETE ");
        let first_unclassified = at("unclassified boss");
        let summary_at = at(summary);
        assert!(
            record_at < first_delete && record_at < first_unclassified,
            "{mode}: the record follows a per-version line (record at {record_at}, \
             first would DELETE at {first_delete}, first unclassified at {first_unclassified}):\n{out}"
        );
        assert!(
            summary_at < first_delete,
            "{mode}: the summary line follows the per-version list:\n{out}"
        );
        assert!(
            record_at < 4096,
            "{mode}: the record starts at byte {record_at}, outside the first 4 KB:\n{out}"
        );
        // Every plan line is still printed, after the verdict.
        contains_all(
            &out,
            &[
                "would DELETE boss:01d0001",
                &format!("would DELETE boss-ci:{}", digest("ci-old1")),
                "unclassified boss:v1",
            ],
            mode,
        );

        // The record names the list file, under the directory the verb
        // was given, and the file holds every per-version line.
        let r = c.record(&out);
        let list_file = PathBuf::from(
            r["list_file"]
                .as_str()
                .unwrap_or_else(|| panic!("{mode}: the record names no list_file: {r}")),
        );
        assert!(
            list_file.starts_with(&c.list_dir),
            "{mode}: {} is not under {}",
            list_file.display(),
            c.list_dir.display()
        );
        assert_eq!(list_file.extension().and_then(|e| e.to_str()), Some("txt"));
        let list = std::fs::read_to_string(&list_file)
            .unwrap_or_else(|e| panic!("{mode}: {} unreadable: {e}", list_file.display()));
        contains_all(
            &list,
            &[
                mode,
                "would DELETE boss:01d0001",
                "would DELETE boss:deadbee",
                &format!("would DELETE boss:{}", digest("orphan-old")),
                &format!("would DELETE boss-ci:{}", forty("01d0001")),
                "unclassified boss:v1",
                "unclassified boss-ci:rust1.96",
            ],
            "the list file",
        );
        no_credential(&list, "the list file");
        if mode == "--for-real" {
            // The outcomes ride the same file: what went, by name.
            contains_all(
                &list,
                &["deleted boss:01d0001", "deleted boss:deadbee"],
                "the list file's outcomes",
            );
        } else {
            assert!(!list.contains("deleted boss"), "a dry run deleted: {list}");
        }
    }
}

/// The file is written BEFORE the first DELETE, and a file that cannot
/// be written is a refusal: a real run whose only full record is the
/// runner's 100 KB window would be the defect again.
#[test]
fn an_unwritable_list_file_refuses_before_any_delete() {
    if !has("jq") {
        return;
    }
    let c = Case::new("list-unwritable");
    // The list "directory" is a plain file, so it cannot be created.
    write_file(&c.list_dir, "not a directory\n");
    for mode in ["--dry-run", "--for-real"] {
        let (rc, out) = c.run(&[mode]);
        assert_eq!(rc, 2, "{mode}: {out}");
        contains_all(
            &out,
            &["REFUSED", "registry-prune", "Nothing was deleted"],
            mode,
        );
        assert!(
            c.deleted().is_empty(),
            "{mode}: a DELETE was sent without the list file: {:?}",
            c.deleted()
        );
        no_credential(&out, mode);
    }
}

// ---------------------------------------------------------------------------
// Unclassified is kept.
// ---------------------------------------------------------------------------

#[test]
fn a_tag_whose_index_cannot_be_read_is_kept_and_so_are_all_orphans() {
    if !has("jq") {
        return;
    }
    let c = Case::new("index-unreadable");
    std::fs::remove_file(c.stub.join("index-boss-deadbee.json")).unwrap();
    let (rc, out) = c.run(&["--for-real"]);
    assert_eq!(rc, 0, "{out}");
    let deleted = c.deleted();
    assert!(
        !deleted.iter().any(|d| d == "boss deadbee"),
        "a tag whose children are unknown was deleted: {deleted:?}"
    );
    assert!(
        !deleted.iter().any(|d| d.contains("orphan-old")),
        "an orphan was deleted while an index was unreadable: {deleted:?}"
    );
    // dead1 is a child of deadbee only; with that index unread it is an
    // orphan, and orphans are unclassified this pass.
    assert!(
        !deleted.iter().any(|d| d.contains(&digest("dead1"))),
        "{deleted:?}"
    );
    // 01d0001 and its children are still classified and go.
    assert!(deleted.iter().any(|d| d == "boss 01d0001"), "{deleted:?}");
    assert!(
        deleted.iter().any(|d| d.contains(&digest("old1"))),
        "{deleted:?}"
    );
    contains_all(
        &out,
        &[
            "unclassified boss:deadbee",
            "404",
            "unclassified boss:sha256:",
        ],
        "the unreadable index is named",
    );
    let r = c.record(&out);
    assert_eq!(r["packages"]["boss"]["index_unreadable"], 1, "{r}");
    assert_eq!(r["packages"]["boss"]["unclassified"], 4, "{r}");
}

#[test]
fn no_bearer_means_no_index_is_readable_and_no_tag_is_deleted() {
    if !has("jq") {
        return;
    }
    let c = Case::new("no-bearer");
    write_file(&c.stub.join("no-token"), "");
    let (rc, out) = c.run(&["--for-real"]);
    assert_eq!(rc, 0, "{out}");
    assert!(c.deleted().is_empty(), "{:?}", c.deleted());
    contains_all(
        &out,
        &["/v2/token", "401", "index"],
        "the token refusal is named",
    );
    let r = c.record(&out);
    assert_eq!(r["packages"]["boss"]["deleted"], 0, "{r}");
    assert_eq!(r["packages"]["boss"]["delete"], 0, "{r}");
}

// ---------------------------------------------------------------------------
// The verb file and the §9a facts.
// ---------------------------------------------------------------------------

#[test]
fn the_verb_file_is_a_mutating_forge_verb_with_mode_and_an_optional_keep_count() {
    let path = repo_root().join("infra/ops/verbs/prune-registry-versions.json");
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(v["hosts"], serde_json::json!(["forge"]));
    assert_eq!(v["argv"], serde_json::json!([SCRIPT, "{1}", "{2}"]));
    let params = v["params"].as_array().unwrap();
    assert_eq!(params.len(), 2);
    assert_eq!(params[0]["name"], "mode");
    assert_eq!(
        params[0]["one_of"],
        serde_json::json!(["--dry-run", "--for-real"])
    );
    assert_eq!(params[1]["name"], "keep_trains");
    assert_eq!(params[1]["optional"], true);
    if has("jq") {
        let matches = |pat: &str, value: &str| -> bool {
            let out = Command::new("jq")
                .args([
                    "-n",
                    "--arg",
                    "v",
                    value,
                    "--arg",
                    "p",
                    pat,
                    "$v | test($p)",
                ])
                .output()
                .expect("jq runs");
            String::from_utf8_lossy(&out.stdout).trim() == "true"
        };
        let pat = params[1]["pattern"].as_str().unwrap();
        for ok in ["1", "10", "250"] {
            assert!(matches(pat, ok), "keep pattern refuses {ok}");
        }
        for bad in ["0", "01", "-1", "1000", "ten", "1 0", ""] {
            assert!(!matches(pat, bad), "keep pattern admits {bad:?}");
        }
    }
    let about = v["about"].as_str().unwrap();
    assert!(about.starts_with("MUTATING"), "{about}");
    assert!(about.contains("David"), "about names who authorized it");
    assert!(about.contains("9789a827"), "about names the packet");
    assert!(about.contains("--dry-run"), "about names the rehearsal");
    assert!(
        about.contains("read:package") && about.contains("write:package"),
        "about names the scopes"
    );
    // ~1,400 versions: one listing page per 50, one manifest read per
    // tag, one DELETE per version — minutes, not the runner's 30 s.
    assert!(v["timeout"].as_i64().unwrap() >= 600, "{}", v["timeout"]);
    let script = repo_root().join(SCRIPT);
    use std::os::unix::fs::PermissionsExt;
    assert!(std::fs::metadata(&script).unwrap().permissions().mode() & 0o111 != 0);
}

/// §9a: the registry and the stamp file the script defaults to are the
/// converge's — the rollback target is read from the same file the
/// deploy runner writes (and the watchdog rolls to by name; that
/// script is not read here because the gate's scope self-test holds it
/// up as an example of a file no crate reads).
#[test]
fn the_registry_and_the_stamp_are_the_ones_the_converge_declares() {
    let src = std::fs::read_to_string(repo_root().join(SCRIPT)).unwrap();
    let runner =
        std::fs::read_to_string(repo_root().join("infra/forge/cluster-deploy-runner.sh")).unwrap();
    let want = |s: &str| assert!(src.contains(s), "script lacks `{s}`");
    want("${BOSS_FORGE_REGISTRY:-10.20.0.15:3000/david/boss}");
    assert!(runner.contains("${BOSS_FORGE_REGISTRY:-10.20.0.15:3000/david/boss}"));
    want("${BOSS_FORGE_LAST_BUILT:-");
    want(".boss-last-built");
    assert!(runner.contains("${BOSS_FORGE_LAST_BUILT:-$HOME/.boss-last-built}"));
    // The landed shas come from the one lib the sweep reads, not a copy.
    want("landed-train-shas.lib.sh");
    want("landed_train_shas ");
    // No trace: `set -x` would print the curl config's header.
    assert!(
        !src.lines()
            .any(|l| !l.trim_start().starts_with('#') && l.contains("set -x")),
        "the script traces (set -x) — a trace prints the credential"
    );
    // The ops runner runs verbs as root with no HOME; the docker login
    // is the checkout owner's, read off the directory, never $HOME.
    assert!(
        !src.lines()
            .any(|l| !l.trim_start().starts_with('#') && l.contains("$HOME")),
        "the script reads $HOME (the ops runner has none)"
    );
}
