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
//!     `not-ready` — and the runner records the line on the packet;
//!   * a SKIPPED instance's hostname is served by the SOURCE instance's
//!     gateway (backlog 40d46042): the converge re-renders the ingress
//!     after the instance secret gate, handing the renderer the packet's
//!     own `instances_skipped` string, applies it, and rolls the
//!     connector exactly when the render changed — cloudflared reads a
//!     local config once at start (2026.9.1 cmd/cloudflared/tunnel/
//!     cmd.go: `prepareTunnelConfig` parses ingress; the fsnotify
//!     watcher exists only for the flagless service mode, and fires on
//!     `Write`, which a ConfigMap's symlink swap never is). The
//!     committed file stays the no-skip render, byte for byte.

use boss_testing::{repo_root, scratch_dir, tunnel_ingress_summary, write_exec, write_file};
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
    run_render_env(tree, args, &[])
}

/// The renderer with extra environment — `BOSS_INSTANCES_SKIPPED` is
/// how the converge hands it the skipped set.
fn run_render_env(tree: &Path, args: &[&str], env: &[(&str, &str)]) -> (i32, String, String) {
    let mut cmd = Command::new("bash");
    cmd.arg(repo_root().join(RENDER))
        .args(args)
        .env("BOSS_CLUSTER_TREE", tree)
        .env_remove("BOSS_INSTANCES_SKIPPED");
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("bash runs the renderer");
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
    std::fs::copy(
        repo_root().join("infra/cluster/tunnel-origins.toml"),
        tree.join("infra/cluster/tunnel-origins.toml"),
    )
    .unwrap();
    tree
}

/// The `[[origin]]` blocks of tunnel-origins.toml as (hostname, service),
/// read independently of the renderer.
fn origins_of(tree: &Path) -> Vec<(String, String)> {
    let text = std::fs::read_to_string(tree.join("infra/cluster/tunnel-origins.toml")).unwrap();
    let mut out = Vec::new();
    let (mut host, mut svc) = (String::new(), String::new());
    for line in text.lines().map(str::trim) {
        if line == "[[origin]]" {
            if !host.is_empty() {
                out.push((host.clone(), svc.clone()));
            }
            host.clear();
            svc.clear();
        } else if let Some(v) = line.strip_prefix("hostname = ") {
            host = v.trim_matches('"').to_string();
        } else if let Some(v) = line.strip_prefix("service = ") {
            svc = v.trim_matches('"').to_string();
        }
    }
    if !host.is_empty() {
        out.push((host, svc));
    }
    out
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
    let origins = origins_of(&repo_root());
    let sites = instances
        .iter()
        .filter(|(_, i)| i.contains_key("site"))
        .count();
    assert_eq!(
        rules.len(),
        instances.len() + sites + origins.len() + 1,
        "one rule per instance, one per declared site (b64c4377), one per declared origin, \
         plus the catch-all; got {rules:?}"
    );
    // In instance order: the hostname, then the instance's site (if
    // any) to the SAME gateway — the site is told apart by Host.
    let mut i = 0;
    for (name, inst) in &instances {
        let host = &inst["hostname"];
        let ns = &inst["namespace"];
        let gateway = format!("http://boss-gateway.{ns}.svc.cluster.local:80");
        assert_eq!(
            rules[i],
            (host.clone(), gateway.clone()),
            "instance [{name}] routes its hostname to its OWN gateway Service, plain HTTP — the \
             edge terminates TLS and the connector is in-cluster"
        );
        i += 1;
        if let Some(site) = inst.get("site") {
            assert_eq!(
                rules[i],
                (site.clone(), gateway),
                "instance [{name}]'s site routes to the same gateway, directly after its hostname"
            );
            i += 1;
        }
    }
    // Then every declared non-instance origin, in file order, AFTER
    // the instances and BEFORE the catch-all (2026-09-16: the IdP's
    // hostname pointed at a deleted tunnel for a day because nothing in
    // the tree routed it; see infra/cluster/tunnel-origins.toml).
    for (j, (host, svc)) in origins.iter().enumerate() {
        assert_eq!(
            rules[instances.len() + sites + j],
            (host.clone(), svc.clone()),
            "declared origin [{host}] routes to its declared service"
        );
    }
    assert!(
        origins.iter().any(|(h, _)| h == "id.algedonic.dev"),
        "the identity provider is a declared origin: {origins:?}"
    );
    let idp = out
        .lines()
        .skip_while(|l| !l.contains("- hostname: id.algedonic.dev"))
        .take(5)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        idp.contains("originServerName: id.algedonic.dev") && idp.contains("noTLSVerify: true"),
        "Kanidm terminates its own TLS with a Cloudflare Origin CA cert: the connector presents \
         the public name and does not verify the cert it cannot trust:\n{idp}"
    );
    assert_eq!(
        rules.last().unwrap(),
        &(String::new(), "http_status:404".to_string()),
        "the last rule is the catch-all cloudflared requires — a hostname the tunnel is \
         not declared for answers 404 at the edge, never the first instance's gateway"
    );
    // A declared origin on an instance's hostname is one route
    // answering two things; refused by name.
    let tree = fixture("origin-collides");
    let toml = read("infra/cluster/tunnel-origins.toml").replace(
        "hostname = \"id.algedonic.dev\"",
        "hostname = \"boss.algedonic.dev\"",
    );
    write_file(&tree.join("infra/cluster/tunnel-origins.toml"), &toml);
    let (rc, _, err) = run_render(&tree, &[]);
    assert_eq!(rc, REFUSED, "{err}");
    assert!(err.contains("boss.algedonic.dev"), "{err}");
    // An origin with no service is a route to nothing; refused.
    let tree = fixture("origin-no-service");
    let toml = read("infra/cluster/tunnel-origins.toml").replace("service = ", "svc_was = ");
    write_file(&tree.join("infra/cluster/tunnel-origins.toml"), &toml);
    let (rc, _, err) = run_render(&tree, &[]);
    assert_eq!(rc, REFUSED, "{err}");
    assert!(err.contains("hostname and service"), "{err}");
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
    // Since 0b7804f3 the read and the recording are observe_connector's
    // (cluster-deploy-lib.sh), so the unchanged tick records the same
    // fields; every_converge_tick_observes_the_connector.rs holds the
    // lib to recording `cloudflared`. What this pins is the deploying
    // call's position.
    let read_at = after
        .find("observe_connector \"$K\"")
        .expect("the runner reads the connector after the roll");
    assert!(
        after[read_at..read_at + 200].contains(" deploy\n"),
        "tagged as the deploying tick's observation"
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

// --- a skipped instance is served by its source (backlog 40d46042) ------

/// The runner's `instances_skipped` field as the apply loop builds it —
/// the ONE string the packet carries, the manifests check is handed,
/// and now the renderer reads (one definition, three readers).
const SKIPPED: &str = "boss-playground (secrets absent: boss-secrets, boss-oidc)";
const SOURCE_GATEWAY: &str = "http://boss-gateway.boss.svc.cluster.local:80";
const SKIP_COMMENT: &str =
    "# [playground] skipped: secrets absent — served by boss until provisioned";
const OWN_COMMENT: &str = "# [playground] — its own gateway, in its own namespace";

#[test]
fn a_skipped_instances_hostname_is_served_by_the_source_gateway_until_provisioned() {
    // Measured 2026-09-16 07:4x: the rotation moved playground.algedonic.dev
    // to the in-cluster tunnel, whose ingress routed it to a gateway in a
    // namespace the converge had SKIPPED (secrets absent) — the hostname
    // reached Cloudflare Access and then nothing. With the skipped set,
    // the hostname goes to the SOURCE instance's gateway, and the comment
    // says why, so the render reads as what it is.
    let (rc, out, err) = run_render_env(&repo_root(), &[], &[("BOSS_INSTANCES_SKIPPED", SKIPPED)]);
    assert_eq!(rc, 0, "{err}");
    let rules = ingress_of(&out);
    let instances = instances_of(&repo_root());
    let origins = origins_of(&repo_root());
    let sites = instances
        .iter()
        .filter(|(_, i)| i.contains_key("site"))
        .count();
    assert_eq!(
        rules.len(),
        instances.len() + sites + origins.len() + 1,
        "{rules:?}"
    );
    assert_eq!(
        rules[0],
        ("boss.algedonic.dev".to_string(), SOURCE_GATEWAY.to_string()),
        "the source instance still routes to its own gateway"
    );
    // prod's site sits between prod and the playground (b64c4377).
    assert_eq!(
        rules[1],
        ("www.algedonic.dev".to_string(), SOURCE_GATEWAY.to_string()),
        "the source's site routes to the source's gateway"
    );
    assert_eq!(
        rules[2],
        (
            "playground.algedonic.dev".to_string(),
            SOURCE_GATEWAY.to_string()
        ),
        "the skipped instance's hostname is served by the source's gateway, not by a \
         namespace with nothing running in it"
    );
    assert!(out.contains(SKIP_COMMENT), "the comment names why: {out}");
    assert!(!out.contains(OWN_COMMENT), "{out}");
    assert_eq!(
        rules.last().unwrap(),
        &(String::new(), "http_status:404".to_string())
    );
    assert_ne!(
        out,
        read(CONFIG),
        "the skip render is not the committed one"
    );

    // The same call with nothing skipped IS the committed file: the
    // converge re-renders on every run, and an applied instance flips
    // back to its own gateway with no other change.
    let (rc, out, err) = run_render_env(&repo_root(), &[], &[("BOSS_INSTANCES_SKIPPED", "")]);
    assert_eq!(rc, 0, "{err}");
    assert_eq!(out, read(CONFIG));

    // The packet's line, from the same loop that renders the rules —
    // `hostname → namespace` per instance, the skip and its reason in
    // parentheses — so the field cannot say one thing and the rules another.
    let (rc, out, err) = run_render_env(
        &repo_root(),
        &["--summary"],
        &[("BOSS_INSTANCES_SKIPPED", SKIPPED)],
    );
    assert_eq!(rc, 0, "{err}");
    assert_eq!(
        out.trim(),
        "boss.algedonic.dev → boss; www.algedonic.dev → boss (site); playground.algedonic.dev → boss (boss-playground skipped: secrets absent); id.algedonic.dev → https://10.20.0.31:443 (origin); dev.algedonic.dev → ssh://boss-dev-ssh.boss-dev.svc.cluster.local:22 (origin)"
    );
    let (rc, out, err) = run_render_env(&repo_root(), &["--summary"], &[]);
    assert_eq!(rc, 0, "{err}");
    assert_eq!(
        out.trim(),
        "boss.algedonic.dev → boss; www.algedonic.dev → boss (site); playground.algedonic.dev → boss-playground; id.algedonic.dev → https://10.20.0.31:443 (origin); dev.algedonic.dev → ssh://boss-dev-ssh.boss-dev.svc.cluster.local:22 (origin)",
        "applied: the field says the hostname is its own instance's again"
    );

    // A skipped set naming a namespace no instance declares is not an
    // instance of this tree — nothing to reroute, nothing to refuse.
    let (rc, out, err) = run_render_env(
        &repo_root(),
        &[],
        &[(
            "BOSS_INSTANCES_SKIPPED",
            "boss-elsewhere (secrets absent: x)",
        )],
    );
    assert_eq!(rc, 0, "{err}");
    assert_eq!(out, read(CONFIG));
}

#[test]
fn the_source_cannot_be_skipped_and_the_committed_file_is_never_the_skip_render() {
    // The source is applied with no secret gate, so the runner never
    // names it skipped; a caller that does has nothing to serve the
    // other hostnames from, and is refused rather than routed to a dark
    // namespace.
    let (rc, _, err) = run_render_env(
        &repo_root(),
        &[],
        &[("BOSS_INSTANCES_SKIPPED", "boss (secrets absent: boss-oidc)")],
    );
    assert_eq!(rc, REFUSED, "{err}");
    assert!(err.contains("source") && err.contains("[prod]"), "{err}");

    // --check and --write are about the COMMITTED file, which is the
    // no-skip render by definition (the byte-identity test above); a
    // skipped set on either is a call that would commit a converge-time
    // state into the tree.
    for mode in ["--check", "--write"] {
        let tree = fixture(&format!("skip-{}", mode.trim_start_matches('-')));
        let before = std::fs::read_to_string(tree.join(CONFIG)).unwrap();
        let (rc, _, err) = run_render_env(&tree, &[mode], &[("BOSS_INSTANCES_SKIPPED", SKIPPED)]);
        assert_eq!(rc, REFUSED, "{mode} with a skipped set is refused: {err}");
        assert!(err.contains("BOSS_INSTANCES_SKIPPED"), "{err}");
        assert_eq!(
            std::fs::read_to_string(tree.join(CONFIG)).unwrap(),
            before,
            "{mode} wrote nothing"
        );
    }
}

/// The tunnel-ingress stage AND the connector stage that follows it,
/// lifted from the runner between two markers, so the test exercises
/// the shipped text rather than a copy. Both stages, because since
/// 0b7804f3 the ingress map is recorded by the connector stage's
/// observe_connector — the one function the unchanged tick records
/// the same fields through — from the skipped string the ingress
/// stage rendered with.
fn ingress_block() -> String {
    let src = read(RUNNER);
    let start = src
        .find("STAGE=\"tunnel ingress\"")
        .expect("the runner has the tunnel-ingress stage");
    let end = src[start..]
        .find("rm -rf \"$APPLY_DIR\"")
        .expect("the connector stage ends by discarding the apply directory");
    src[start..start + end].to_string()
}

/// Runs the block with a stub kubectl that keeps what `apply -f -` was
/// fed, answers `patch` from STUB_PATCH, and logs every call; the real
/// renderer runs against the real tree. The connector stage's reads
/// (a client dry run of a manifest the stub has none of, `rollout
/// status`) get the stub's default empty exit 0: no Secret to derive,
/// rolled out — `connected`.
fn run_ingress(
    name: &str,
    skipped: &str,
    patch_answer: &str,
) -> (i32, String, String, serde_json::Value, PathBuf) {
    let dir = scratch_dir(&format!("tunnel-ingress-{name}"));
    let kubectl = dir.join("kubectl");
    write_exec(
        &kubectl,
        r#"#!/usr/bin/env bash
echo "kubectl $*" >> "$STUB_LOG"
case "$*" in
  *"apply -f -"*) cat > "$STUB_APPLIED"; echo "configmap/cloudflared-config configured" ;;
  *"patch "*) echo "deployment.apps/cloudflared $STUB_PATCH" ;;
esac
exit 0
"#,
    );
    let script = format!(
        "set -euo pipefail\nREPO='{repo}'\nSOURCE_NS=boss\nK='{k}'\nKM='{k}'\nKAPPLY='{k}'\nINSTANCES_SKIPPED='{skipped}'\n\
         _stage_done() {{ :; }}\n. '{lib}'\n. '{deploy_lib}'\n{block}\nexit 0\n",
        repo = repo_root().display(),
        k = kubectl.display(),
        lib = repo_root().join("infra/run-summary.sh").display(),
        deploy_lib = repo_root().join(LIB).display(),
        block = ingress_block()
    );
    let summary = dir.join("summary.json");
    let mut cmd = Command::new("bash");
    cmd.arg("-c")
        .arg(script)
        .env("BOSS_RUN_SUMMARY_FILE", &summary)
        .env("STUB_LOG", dir.join("calls"))
        .env("STUB_APPLIED", dir.join("applied.yaml"))
        .env("STUB_PATCH", patch_answer)
        .current_dir(&dir);
    let out = cmd.output().unwrap();
    let recorded = std::fs::read_to_string(&summary)
        .map(|s| serde_json::from_str(&s).unwrap())
        .unwrap_or(serde_json::Value::Null);
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
        recorded,
        dir,
    )
}

/// sha256 of a text, by the same tool the runner uses.
fn sha256_hex(text: &str) -> String {
    let mut child = Command::new("sh")
        .args(["-c", "sha256sum | cut -d' ' -f1"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    // Its exit status is the verdict, not the write (backlog d93cc7d5).
    boss_testing::feed_stdin(&mut child, text.as_bytes());
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "sha256sum: {:?}", out.status);
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
fn the_runner_re_renders_the_ingress_after_the_secret_gate_and_rolls_the_connector_when_it_changed()
{
    // Position: after the apply loop has decided what is skipped, and
    // before the connector is read — so the `cloudflared` field is about
    // the connector serving THIS render — and before the apply directory
    // is discarded.
    let src = read(RUNNER);
    let skipped_at = src
        .find("run_summary_field instances_skipped")
        .expect("the skip rides the packet");
    let ingress_at = src
        .find("STAGE=\"tunnel ingress\"")
        .expect("the runner has the tunnel-ingress stage");
    let connector_at = src
        .find("observe_connector \"$K\" \"$KM\"")
        .expect("the runner reads the connector (through the lib, since 0b7804f3)");
    let discard = src
        .find("rm -rf \"$APPLY_DIR\"")
        .expect("the runner discards the apply directory");
    assert!(
        skipped_at < ingress_at,
        "re-rendered AFTER the instance secret gate"
    );
    assert!(
        ingress_at < connector_at,
        "the connector is read after its ingress is applied"
    );
    assert!(
        connector_at < discard,
        "and while /manifests is still mounted"
    );
    // ONE writer: the committed copy is dropped from prod's apply set, so
    // the object never flips no-skip → skip inside a converge and a pod
    // that restarts between the two cannot load the wrong render.
    let prod_apply = src
        .find("apply_instance \"$SOURCE_NS\"")
        .expect("prod is applied");
    let dropped = src
        .find("rm -f \"$APPLY_DIR/$SOURCE_NS/$TUNNEL_CONFIG_FILE\"")
        .expect("the staged ConfigMap is dropped from the apply set");
    assert!(dropped < prod_apply, "dropped BEFORE prod's apply");
    assert!(
        src[..dropped].contains("TUNNEL_CONFIG_FILE=cloudflared-config.yaml"),
        "the file dropped is the rendered ConfigMap"
    );
    let block = ingress_block();
    assert!(
        block.contains("BOSS_INSTANCES_SKIPPED=\"$INSTANCES_SKIPPED\""),
        "the renderer is handed the SAME string the packet carries — the idiom the \
         manifests check already reads: {block}"
    );

    // Skipped: the applied ConfigMap serves the playground from prod, the
    // packet says so, and the connector is rolled because the render
    // differs from what its pods loaded.
    let (rc, out, err, recorded, dir) = run_ingress("skipped", SKIPPED, "patched");
    assert_eq!(rc, 0, "{out}\n{err}");
    let applied = std::fs::read_to_string(dir.join("applied.yaml")).unwrap();
    let rules = ingress_of(&applied);
    // rules[1] is prod's site (b64c4377); the playground follows it.
    assert_eq!(
        rules[2],
        (
            "playground.algedonic.dev".to_string(),
            SOURCE_GATEWAY.to_string()
        ),
        "{applied}"
    );
    assert!(applied.contains(SKIP_COMMENT), "{applied}");
    assert_eq!(
        recorded["tunnel_ingress"],
        tunnel_ingress_summary(SKIPPED),
        "{recorded}"
    );
    // The deploying tick still records the connector beside the map,
    // and says which tick looked (0b7804f3).
    assert_eq!(recorded["cloudflared"], "connected", "{recorded}");
    assert_eq!(recorded["observed_on"], "deploy", "{recorded}");
    let sha = sha256_hex(&applied);
    let calls = std::fs::read_to_string(dir.join("calls")).unwrap();
    let patch = calls
        .lines()
        .find(|l| l.contains(" patch "))
        .unwrap_or_else(|| panic!("the connector's pod template is patched: {calls}"));
    assert!(
        patch.contains("deploy") && patch.contains("cloudflared") && patch.contains("-n boss"),
        "{patch}"
    );
    assert!(
        patch.contains(&sha),
        "the patch carries the sha256 of the APPLIED render, so the pods roll exactly when \
         the ingress they loaded is not this one: {patch}"
    );
    let apply_at = calls.find("apply -f -").unwrap();
    let patch_at = calls.find(" patch ").unwrap();
    assert!(
        apply_at < patch_at,
        "the ConfigMap is applied before the pods that mount it roll"
    );
    assert_eq!(
        recorded["tunnel_ingress_render"],
        format!("{} (connector rolled)", &sha[..12]),
        "{recorded}"
    );

    // Applied (nothing skipped): the applied ConfigMap IS the committed
    // file, the field says the hostname is its own instance's, and a
    // pod template already on this render is left alone.
    let (rc, out, err, recorded, dir) = run_ingress("applied", "", "patched (no change)");
    assert_eq!(rc, 0, "{out}\n{err}");
    let applied = std::fs::read_to_string(dir.join("applied.yaml")).unwrap();
    assert_eq!(
        applied,
        read(CONFIG),
        "with nothing skipped the converge applies the committed render"
    );
    assert_eq!(recorded["tunnel_ingress"], tunnel_ingress_summary(""));
    let sha = sha256_hex(&applied);
    assert_eq!(
        recorded["tunnel_ingress_render"],
        format!("{} (connector unchanged)", &sha[..12]),
        "{recorded}"
    );
    let calls = std::fs::read_to_string(dir.join("calls")).unwrap();
    assert!(
        !calls.contains("rollout restart"),
        "no unconditional restart: the template hash is the only trigger: {calls}"
    );
}
