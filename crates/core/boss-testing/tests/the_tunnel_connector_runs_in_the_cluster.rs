//! The Cloudflare Tunnel connector is a DECLARED cluster workload
//! (backlog 5a2bb0ce; design 4c565f8c, decided by David 2026-09-16):
//! every public hostname reaches the cluster through a tunnel whose
//! connector runs IN the cluster, in config-file mode, with the ingress
//! map rendered from the tree. `infra/cluster/render-tunnel-config.sh`
//! and the lib function the converge reads the connector with are RUN
//! here — against the tree and against fixture trees — so every verdict
//! below is one the scripts actually reached. Nothing here touches a
//! cluster.
//!
//! WHY. Until this car, cloudflared ran on boss-gcp as a hand-written
//! unit with its token inline (leaked into two ops-requests), undeclared
//! anywhere in the tree, and every public request crossed a VM and a
//! WireGuard hop that exist for other reasons. The connector is now
//! `infra/cluster/manifests/cloudflared.yaml`, a pipeline manifest (one
//! tunnel, in prod, serving every instance), and its ingress rules are
//! `infra/cluster/manifests/cloudflared-config.yaml` — RENDERED from
//! `infra/cluster/instances.toml`, because every hostname the tunnel
//! routes is an instance's hostname and every origin is that instance's
//! gateway Service: a second file restating both would be the §9a pair
//! that drifts on the first car that renames a hostname.
//!
//! What each case pins:
//!   * the roster classifies both files `pipeline` — read here
//!     independently of the renderer, as render_instance_sh.rs does;
//!   * the committed ConfigMap IS the render, byte for byte (a fact
//!     that lives twice gets an equality test), and `--check` sees a
//!     drifted tree;
//!   * the render routes every instance's hostname to its own gateway,
//!     in instance order, and ends on the mandatory `http_status:404`
//!     catch-all — and refuses a tree whose gateway Service it cannot
//!     find, or whose instances share a hostname;
//!   * the connector mounts the credentials Secret by name, read-only,
//!     and never `optional` — and no document tells a human to mint it
//!     by hand any more (51c98681: the converge creates the object
//!     empty, the broker fills it); the registry row and the ConfigMap
//!     still agree on the key;
//!   * the image is pinned by tag AND digest;
//!   * the converge reads the connector after the roll: the Secret's
//!     name is DERIVED from the rendered manifest through the lib's own
//!     `manifest_secrets`, an absent Secret reads `skipped (secret
//!     absent: …)`, a Ready rollout reads `connected`, a stalled one
//!     `not-ready` — and the runner records the line on the packet.

use boss_testing::{repo_root, scratch_dir, write_exec, write_file};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

const RENDER: &str = "infra/cluster/render-tunnel-config.sh";
const ROSTER: &str = "infra/cluster/instance-manifests.txt";
const INSTANCES: &str = "infra/cluster/instances.toml";
const MANIFESTS: &str = "infra/cluster/manifests";
const CONNECTOR: &str = "infra/cluster/manifests/cloudflared.yaml";
const CONFIG: &str = "infra/cluster/manifests/cloudflared-config.yaml";
const README: &str = "infra/cluster/manifests/README.md";
const LIB: &str = "infra/forge/cluster-deploy-lib.sh";
const RUNNER: &str = "infra/forge/cluster-deploy-runner.sh";
const SCHEMA: &str = "infra/postgres/schema";

const SECRET: &str = "cloudflare-tunnel-credentials";
const SECRET_KEY: &str = "credentials.json";
/// The hand mint this Secret USED to need (5a2bb0ce) and no longer
/// may be told to a human (51c98681): the converge creates the object
/// empty and the broker fills it.
const HAND_MINT: &str = "create secret generic cloudflare-tunnel-credentials --from-file";

/// The exit code the renderer uses for a refusal — the same as
/// render-instance.sh, so a caller can tell "refused, reason on
/// stderr" from a crash.
const REFUSED: i32 = 2;

fn read(rel: &str) -> String {
    std::fs::read_to_string(repo_root().join(rel))
        .unwrap_or_else(|e| panic!("the tree carries {rel}: {e}"))
}

/// `key = "value"` lines under `[name]` headers — the shell-readable
/// subset both renderers parse. Sections in file order.
fn instances_of(tree: &Path) -> Vec<(String, BTreeMap<String, String>)> {
    let text = std::fs::read_to_string(tree.join(INSTANCES)).expect("instances.toml");
    let mut out: Vec<(String, BTreeMap<String, String>)> = Vec::new();
    for line in text.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            out.push((name.to_string(), BTreeMap::new()));
            continue;
        }
        if let (Some(last), Some((k, v))) = (out.last_mut(), line.split_once('=')) {
            last.1
                .insert(k.trim().to_string(), v.trim().trim_matches('"').to_string());
        }
    }
    out
}

fn run_render(tree: &Path, args: &[&str]) -> (i32, String, String) {
    let out = Command::new("bash")
        .arg(repo_root().join(RENDER))
        .args(args)
        .env("BOSS_CLUSTER_TREE", tree)
        .output()
        .expect("bash runs the renderer");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

/// A fixture tree: the repository's own manifests directory and
/// instance list copied into scratch, so a case can perturb one thing
/// and watch the refusal it earns.
fn fixture(case: &str) -> PathBuf {
    let tree = scratch_dir(&format!("tunnel-config-{case}")).join("tree");
    let dir = tree.join(MANIFESTS);
    std::fs::create_dir_all(&dir).unwrap();
    for e in std::fs::read_dir(repo_root().join(MANIFESTS)).unwrap() {
        let e = e.unwrap();
        std::fs::copy(e.path(), dir.join(e.file_name())).unwrap();
    }
    std::fs::copy(repo_root().join(INSTANCES), tree.join(INSTANCES)).unwrap();
    tree
}

/// The `ingress:` rules of a rendered config, as (hostname, service)
/// pairs in order; a catch-all has an empty hostname.
fn ingress_of(config_yaml: &str) -> Vec<(String, String)> {
    let mut rules: Vec<(String, String)> = Vec::new();
    let mut in_ingress = false;
    for line in config_yaml.lines() {
        let t = line.trim_start();
        if t.starts_with("ingress:") {
            in_ingress = true;
            continue;
        }
        if !in_ingress {
            continue;
        }
        if let Some(h) = t.strip_prefix("- hostname: ") {
            rules.push((h.trim().to_string(), String::new()));
        } else if let Some(s) = t.strip_prefix("- service: ") {
            rules.push((String::new(), s.trim().to_string()));
        } else if let Some(s) = t.strip_prefix("service: ") {
            rules.last_mut().expect("a service follows its hostname").1 = s.trim().to_string();
        }
    }
    rules
}

#[test]
fn the_connector_and_its_config_are_pipeline_manifests() {
    let roster: BTreeMap<String, String> = read(ROSTER)
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| {
            let mut it = l.split_whitespace();
            (
                it.next().unwrap().to_string(),
                it.next().unwrap_or("").to_string(),
            )
        })
        .collect();
    for file in ["cloudflared.yaml", "cloudflared-config.yaml"] {
        assert!(
            repo_root().join(MANIFESTS).join(file).is_file(),
            "{MANIFESTS}/{file} is in the tree"
        );
        assert_eq!(
            roster.get(file).map(String::as_str),
            Some("pipeline"),
            "{ROSTER}: {file} is `pipeline` — ONE tunnel, in prod, serving every instance; \
             a per-instance render would start a second connector per namespace on the \
             same credentials"
        );
    }
}

#[test]
fn the_committed_config_is_the_render_and_check_sees_a_drifted_tree() {
    let (rc, out, err) = run_render(&repo_root(), &[]);
    assert_eq!(rc, 0, "the tree renders: {err}");
    assert_eq!(
        out,
        read(CONFIG),
        "{CONFIG} is not what {RENDER} renders from {INSTANCES} — run `{RENDER} --write`"
    );
    let (rc, _, err) = run_render(&repo_root(), &["--check"]);
    assert_eq!(rc, 0, "--check is clean on the tree: {err}");

    // A tree whose instance list moved on (a hostname renamed — car 2's
    // www.algedonic.dev is exactly this edit) without the render.
    let tree = fixture("drift");
    let toml = read(INSTANCES).replace("playground.algedonic.dev", "sandbox.algedonic.dev");
    assert_ne!(toml, read(INSTANCES), "the fixture perturbs a hostname");
    write_file(&tree.join(INSTANCES), &toml);
    let (rc, _, err) = run_render(&tree, &["--check"]);
    assert_eq!(
        rc, REFUSED,
        "a drifted tree is refused, not rendered around: {err}"
    );
    assert!(
        err.contains("cloudflared-config.yaml") && err.contains("--write"),
        "the refusal names the file and the fix: {err}"
    );
    // And --write is the fix: after it, --check is clean and the file
    // carries the new hostname.
    let (rc, _, err) = run_render(&tree, &["--write"]);
    assert_eq!(rc, 0, "{err}");
    let (rc, _, err) = run_render(&tree, &["--check"]);
    assert_eq!(rc, 0, "{err}");
    assert!(
        std::fs::read_to_string(tree.join(CONFIG))
            .unwrap()
            .contains("hostname: sandbox.algedonic.dev")
    );
}

#[test]
fn every_instance_hostname_routes_to_its_own_gateway_and_the_catch_all_is_last() {
    let (rc, out, err) = run_render(&repo_root(), &[]);
    assert_eq!(rc, 0, "{err}");
    let rules = ingress_of(&out);
    let instances = instances_of(&repo_root());
    assert!(instances.len() >= 2, "prod and the playground are declared");
    assert_eq!(
        rules.len(),
        instances.len() + 1,
        "one rule per instance plus the catch-all; got {rules:?}"
    );
    for (i, (name, inst)) in instances.iter().enumerate() {
        let host = &inst["hostname"];
        let ns = &inst["namespace"];
        assert_eq!(
            rules[i],
            (
                host.clone(),
                format!("http://boss-gateway.{ns}.svc.cluster.local:80")
            ),
            "instance [{name}] routes its hostname to its OWN gateway Service, plain HTTP — the \
             edge terminates TLS and the connector is in-cluster"
        );
    }
    assert_eq!(
        rules.last().unwrap(),
        &(String::new(), "http_status:404".to_string()),
        "the last rule is the catch-all cloudflared requires — a hostname the tunnel is \
         not declared for answers 404 at the edge, never the first instance's gateway"
    );
    // The connector's own facts ride the same file: it resolves the
    // tunnel from the credentials file and serves /ready for the probe.
    assert!(out.contains("credentials-file: /etc/cloudflared-creds/credentials.json"));
    assert!(out.contains("metrics: 0.0.0.0:2000"));

    // Two instances on one hostname is a route that answers the wrong
    // instance; refused by name.
    let tree = fixture("duplicate-hostname");
    let toml = read(INSTANCES).replace("playground.algedonic.dev", "boss.algedonic.dev");
    write_file(&tree.join(INSTANCES), &toml);
    let (rc, _, err) = run_render(&tree, &[]);
    assert_eq!(rc, REFUSED, "{err}");
    assert!(err.contains("boss.algedonic.dev"), "{err}");

    // A tree whose boss.yaml no longer declares the gateway Service on
    // :80 would render routes to nothing; refused rather than guessed.
    let tree = fixture("no-gateway");
    let boss = std::fs::read_to_string(tree.join(MANIFESTS).join("boss.yaml")).unwrap();
    write_file(
        &tree.join(MANIFESTS).join("boss.yaml"),
        &boss.replace("name: boss-gateway", "name: boss-front"),
    );
    let (rc, _, err) = run_render(&tree, &[]);
    assert_eq!(rc, REFUSED, "{err}");
    assert!(err.contains("boss-gateway"), "{err}");
}

#[test]
fn the_connector_mounts_the_credentials_secret_by_name_and_the_mint_shape_is_spelled_once() {
    let manifest = read(CONNECTOR);
    // The Secret, by name, never optional: a connector without its
    // credentials must wait visibly (ContainerCreating), not start
    // and serve nothing.
    let vol = manifest
        .lines()
        .find(|l| l.contains("secretName:"))
        .expect("the connector mounts a Secret volume");
    assert!(vol.contains(&format!("secretName: {SECRET}")), "{vol}");
    assert!(!vol.contains("optional"), "never optional: {vol}");
    assert!(
        manifest.contains("mountPath: /etc/cloudflared-creds")
            && manifest.contains("readOnly: true"),
        "mounted read-only where config.yaml's credentials-file points"
    );
    assert!(
        manifest.contains("--config") && manifest.contains("/etc/cloudflared/config.yaml"),
        "config-file mode: the ingress map is the tree's, not the dashboard's"
    );
    // The ConfigMap the connector mounts is the one the render writes.
    let config = read(CONFIG);
    let cm_name = config
        .lines()
        .find_map(|l| l.trim().strip_prefix("name: "))
        .expect("the rendered ConfigMap has a name");
    assert!(
        manifest.contains(&format!("configMap: {{name: {cm_name}}}")),
        "the Deployment mounts ConfigMap `{cm_name}`"
    );
    // The image: a tag a human reads AND a digest the kubelet enforces.
    let image = manifest
        .lines()
        .find_map(|l| l.trim().strip_prefix("image: "))
        .expect("an image line");
    let (tagged, digest) = image
        .split_once("@sha256:")
        .unwrap_or_else(|| panic!("pinned by digest: {image}"));
    assert!(
        tagged.starts_with("cloudflare/cloudflared:20"),
        "pinned by a release tag too: {image}"
    );
    assert!(
        digest.len() == 64 && digest.chars().all(|c| c.is_ascii_hexdigit()),
        "{image}"
    );

    // NO hand mint shape anywhere (backlog 51c98681): the converge
    // creates the object empty and the broker fills it, so a `create
    // secret … --from-file` instruction is not merely stale, it fails
    // AlreadyExists against the object the converge made. The header
    // and the README name the two halves instead; the registry row's
    // storage line and the ConfigMap's credentials-file path still
    // agree on the key.
    for (doc, text) in [(CONNECTOR, manifest.clone()), (README, read(README))] {
        assert!(
            !text.contains(HAND_MINT),
            "{doc} still tells a human to mint the Secret by hand"
        );
        assert!(
            text.contains("ensure_declared_secrets") && text.contains("secrets_declared"),
            "{doc} names the converge stage that creates the object and its packet field"
        );
        assert!(
            text.contains("broker-rotates-the-cloudflare-tunnel"),
            "{doc} names the rule that fills it"
        );
    }
    let migration = std::fs::read_dir(repo_root().join(SCHEMA))
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| std::fs::read_to_string(p).is_ok_and(|s| s.contains(&format!("'{SECRET}'"))))
        .unwrap_or_else(|| panic!("a migration under {SCHEMA} declares the registry row"));
    let row = std::fs::read_to_string(&migration).unwrap();
    assert!(
        row.contains(&format!("k8s Secret boss/{SECRET} key {SECRET_KEY}")),
        "{}: storage_location names the Secret and key",
        migration.display()
    );
    assert!(
        row.contains("'cloudflare-tunnel-credentials'") && row.contains("cloudflared"),
        "kind and consumer"
    );
    assert!(
        config.contains(&format!(
            "credentials-file: /etc/cloudflared-creds/{SECRET_KEY}"
        )),
        "the connector reads the key the Secret is minted with"
    );
}

/// A stub kubectl for the lib: dry-run cats the JSON fixture, `get
/// secret` answers from a list of present names, `rollout status`
/// answers from STUB_ROLLOUT.
struct Case {
    root: PathBuf,
    kubectl: PathBuf,
    manifest: PathBuf,
    present: PathBuf,
}

impl Case {
    fn new(name: &str) -> Self {
        let root = scratch_dir(&format!("tunnel-connector-{name}"));
        std::fs::create_dir_all(&root).unwrap();
        let manifest = root.join("cloudflared.yaml");
        let present = root.join("present");
        write_file(&present, "");
        // Shaped like the connector: one Secret volume, one ConfigMap
        // volume. The Secret's NAME is deliberately not the real one —
        // a `skipped` line naming it proves the lib derived it from the
        // manifest rather than knowing it.
        write_file(
            &manifest,
            r#"{"kind":"Deployment","metadata":{"name":"cloudflared","namespace":"boss"},"spec":{"template":{"spec":{
  "containers":[{"name":"cloudflared","volumeMounts":[{"name":"creds","mountPath":"/etc/cloudflared-creds"}]}],
  "volumes":[
    {"name":"creds","secret":{"secretName":"tunnel-creds-fixture"}},
    {"name":"config","configMap":{"name":"cloudflared-config"}}]}}}}
"#,
        );
        let kubectl = root.join("kubectl");
        write_exec(
            &kubectl,
            r#"#!/usr/bin/env bash
echo "kubectl $*" >> "$STUB_LOG"
all="$*"
last="${all##* }"
case "$all" in
  *"create --dry-run=client -o json -f "*) cat "$last" ;;
  *"get secret -n "*)
    if grep -qx -- "$last" "$STUB_PRESENT"; then echo "NAME  TYPE  DATA"; exit 0; fi
    echo "Error from server (NotFound): secrets \"$last\" not found" >&2; exit 1 ;;
  *"rollout status deploy/"*)
    [ "${STUB_ROLLOUT:-ok}" = ok ] && { echo 'deployment "cloudflared" successfully rolled out'; exit 0; }
    echo "error: timed out waiting for the condition" >&2; exit 1 ;;
esac
exit 0
"#,
        );
        Self {
            root,
            kubectl,
            manifest,
            present,
        }
    }

    fn run(&self, env: &[(&str, &str)]) -> (i32, String, String) {
        let script = format!(
            ". '{}'\nconnector_status \"$K\" \"$K\" boss \"$M\" cloudflared\n",
            repo_root().join(LIB).display()
        );
        let mut cmd = Command::new("bash");
        cmd.arg("-c")
            .arg(&script)
            .env("STUB_LOG", self.root.join("calls"))
            .env("STUB_PRESENT", &self.present)
            .env("K", &self.kubectl)
            .env("M", &self.manifest);
        for (k, v) in env {
            cmd.env(k, v);
        }
        let out = cmd.output().unwrap();
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).to_string(),
            String::from_utf8_lossy(&out.stderr).to_string(),
        )
    }
}

fn require_jq() {
    let ok = Command::new("sh")
        .args(["-c", "command -v jq >/dev/null 2>&1"])
        .status()
        .is_ok_and(|s| s.success());
    assert!(
        ok,
        "jq is missing, and this suite would otherwise pass by skipping — \
         jq is declared in infra/forge/boss-ci/required-tools.txt"
    );
}

#[test]
fn the_converge_reads_the_connector_as_skipped_connected_or_not_ready() {
    require_jq();
    // The Secret is not minted: skipped, naming the Secret the MANIFEST
    // requires — derived, so the fixture's name is what comes back.
    let c = Case::new("absent");
    let (rc, out, err) = c.run(&[]);
    assert_eq!(rc, 0, "{err}");
    assert_eq!(out.trim(), "skipped (secret absent: tunnel-creds-fixture)");
    let calls = std::fs::read_to_string(c.root.join("calls")).unwrap();
    assert!(
        !calls.contains("rollout status"),
        "no rollout is waited on for pods that cannot start: {calls}"
    );

    // Minted and Ready: connected — cloudflared's readinessProbe is its
    // own /ready, which answers 200 only with a live edge connection.
    let c = Case::new("connected");
    write_file(&c.present, "tunnel-creds-fixture\n");
    let (rc, out, err) = c.run(&[]);
    assert_eq!(rc, 0, "{err}");
    assert_eq!(out.trim(), "connected");

    // Minted, rollout stalls: not-ready, and the converge still
    // returns 0 — the state rides the packet; the car's probe judges it.
    let c = Case::new("not-ready");
    write_file(&c.present, "tunnel-creds-fixture\n");
    let (rc, out, err) = c.run(&[("STUB_ROLLOUT", "stalled")]);
    assert_eq!(rc, 0, "{err}");
    assert_eq!(out.trim(), "not-ready");
}

#[test]
fn the_runner_records_the_connector_on_the_packet_after_the_roll() {
    let src = read(RUNNER);
    let stamped = src
        .find("OUTCOME=\"converged=$HEAD\"")
        .expect("the runner marks the roll real");
    let after = &src[stamped..];
    let read_at = after
        .find("$(connector_status ")
        .expect("the runner reads the connector after the roll");
    assert!(
        after[read_at..].contains("run_summary_field cloudflared"),
        "the line rides the converge packet as `cloudflared`, the field the probe reads"
    );
    // Before the rendered apply directory is discarded: the Secret is
    // derived from the RENDERED manifest, which lives only there.
    let discard = after
        .find("rm -rf \"$APPLY_DIR\"")
        .expect("the runner discards the apply directory");
    assert!(read_at < discard, "read while /manifests is still mounted");
    assert!(
        after[read_at..read_at + 200].contains("cloudflared.yaml"),
        "the connector's own manifest is what the Secret is derived from"
    );
}
