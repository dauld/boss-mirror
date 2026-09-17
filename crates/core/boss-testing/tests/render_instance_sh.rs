//! `infra/cluster/render-instance.sh` is RUN, not read — against the
//! tree's own manifests and against fixture trees, so every verdict
//! below is one the script actually reached. Nothing here touches a
//! cluster.
//!
//! WHY THE RENDERER EXISTS (backlog 07d7549c; design ffc83387, decided
//! by David 2026-09-16). playground.algedonic.dev is a SECOND NAMESPACE
//! on the current cluster, and both it and prod converge on every
//! train. Every manifest under infra/cluster/manifests/ hard-codes
//! `namespace: boss`, one tenant path, `BOSS_SIM_ENABLED=false` and one
//! public hostname; a second namespace as a second copy of the YAML
//! would be the §9a pair that drifts. So there is ONE source — the
//! directory, which IS the prod instance — and an instance is rendered
//! from it at converge time with four parameters substituted, in the
//! idiom `manifests_with_image` already uses for the image sha.
//!
//! What each case pins:
//!   * the roster (`infra/cluster/instance-manifests.txt`) classifies
//!     every manifest in the directory in exactly one set, read here
//!     INDEPENDENTLY of the script — a new file must be classified
//!     (CLAUDE.md §9a: a fact that lives twice gets an equality test);
//!   * the prod render is BYTE-IDENTICAL to the directory, so landing
//!     the renderer changes nothing live until the runner is told to
//!     render a second instance;
//!   * the playground render substitutes exactly the four parameters
//!     (namespace, tenant, sim, hostname) plus the namespace-scoped DNS
//!     names, and drops the LoadBalancer IP pins that only one Service
//!     on the LAN can hold;
//!   * the refusals: an unknown namespace pattern, a tenant outside
//!     examples/, a manifest the roster does not classify, and a source
//!     value the files do not carry (the prod entry drifting from the
//!     files would make every other instance's substitution a no-op
//!     that answers instead of erroring).

use boss_testing::{repo_root, scratch_dir, write_file};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

const SCRIPT: &str = "infra/cluster/render-instance.sh";
const ROSTER: &str = "infra/cluster/instance-manifests.txt";
const INSTANCES: &str = "infra/cluster/instances.toml";
const MANIFESTS: &str = "infra/cluster/manifests";

/// The exit code the renderer uses for a refusal — distinct from 0
/// (rendered) and 1 (something else went wrong), so a caller can tell
/// "this was refused, and the reason is on stderr" from a crash.
const REFUSED: i32 = 2;

/// The roster, read the way a second reader would: `<file> <set>` per
/// line, comments and blanks dropped.
fn roster_of(tree: &Path) -> Vec<(String, String)> {
    std::fs::read_to_string(tree.join(ROSTER))
        .expect("the tree carries infra/cluster/instance-manifests.txt")
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
        .collect()
}

/// Every `*.yaml` basename in a manifests directory.
fn yaml_names(dir: &Path) -> BTreeSet<String> {
    std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
        .filter(|n| n.ends_with(".yaml"))
        .collect()
}

/// `key = "value"` / `key = true` lines under `[name]` headers — the
/// shell-readable subset the script itself parses.
/// The directory under /opt/boss an instance's pod reads: `tenant_dir`
/// for an image-sourced instance, `tenant` (the delivered ConfigMap
/// mount) for a repo-sourced one — prod, since the flip (2026-09-16).
fn tenant_of(inst: &BTreeMap<String, String>) -> String {
    inst.get("tenant_dir")
        .cloned()
        .unwrap_or_else(|| "tenant".to_string())
}

fn instances_of(tree: &Path) -> BTreeMap<String, BTreeMap<String, String>> {
    let text = std::fs::read_to_string(tree.join(INSTANCES))
        .expect("the tree carries infra/cluster/instances.toml");
    let mut out: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    let mut section = String::new();
    for line in text.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            section = name.to_string();
            out.entry(section.clone()).or_default();
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            out.entry(section.clone())
                .or_default()
                .insert(k.trim().to_string(), v.trim().trim_matches('"').to_string());
        }
    }
    out
}

fn run(tree: &Path, args: &[&str]) -> (i32, String, String) {
    let out = Command::new("bash")
        .arg(repo_root().join(SCRIPT))
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

/// A fixture tree: the repository's own manifests, roster and instance
/// list copied into scratch, so a case can perturb one thing and watch
/// the refusal it earns.
fn fixture(case: &str) -> PathBuf {
    let tree = scratch_dir(&format!("render-instance-{case}")).join("tree");
    let dir = tree.join(MANIFESTS);
    std::fs::create_dir_all(&dir).unwrap();
    for name in yaml_names(&repo_root().join(MANIFESTS)) {
        std::fs::copy(repo_root().join(MANIFESTS).join(&name), dir.join(&name)).unwrap();
    }
    std::fs::copy(repo_root().join(ROSTER), tree.join(ROSTER)).unwrap();
    std::fs::copy(repo_root().join(INSTANCES), tree.join(INSTANCES)).unwrap();
    // The tenant directories the instance list points at must exist in
    // the tree with a manifest — the renderer refuses one it cannot
    // find (`tenant_dir`, since f4f5c387; the examples keep the manifest
    // at seeds/tenant.toml).
    for inst in instances_of(&repo_root()).values() {
        if let Some(d) = inst.get("tenant_dir") {
            let t = format!("{d}/seeds/tenant.toml");
            let dst = tree.join(&t);
            std::fs::create_dir_all(dst.parent().unwrap()).unwrap();
            std::fs::copy(repo_root().join(&t), &dst).unwrap();
        }
    }
    tree
}

#[test]
fn the_roster_classifies_every_manifest_exactly_once() {
    let roster = roster_of(&repo_root());
    let mut seen: BTreeMap<String, usize> = BTreeMap::new();
    for (file, set) in &roster {
        assert!(
            set == "instance" || set == "pipeline",
            "{ROSTER}: {file} is in set `{set}` — the sets are `instance` and `pipeline`"
        );
        *seen.entry(file.clone()).or_default() += 1;
    }
    let dups: Vec<_> = seen
        .iter()
        .filter(|(_, n)| **n > 1)
        .map(|(f, _)| f)
        .collect();
    assert!(
        dups.is_empty(),
        "{ROSTER} names these more than once: {dups:?}"
    );
    let in_dir = yaml_names(&repo_root().join(MANIFESTS));
    let in_roster: BTreeSet<String> = seen.keys().cloned().collect();
    let unclassified: Vec<_> = in_dir.difference(&in_roster).collect();
    let phantom: Vec<_> = in_roster.difference(&in_dir).collect();
    assert!(
        unclassified.is_empty(),
        "{MANIFESTS} holds manifests {ROSTER} does not classify: {unclassified:?} — \
         add each as `instance` (rendered per instance) or `pipeline` (applied once, in prod)"
    );
    assert!(
        phantom.is_empty(),
        "{ROSTER} names manifests that are not in {MANIFESTS}: {phantom:?}"
    );
    // The script's own reading of the roster agrees with this one.
    let (rc, out, err) = run(&repo_root(), &["--roster"]);
    assert_eq!(rc, 0, "--roster: {err}");
    let printed: BTreeSet<(String, String)> = out
        .lines()
        .map(|l| {
            let (f, s) = l.split_once('\t').expect("file<TAB>set");
            (f.to_string(), s.to_string())
        })
        .collect();
    assert_eq!(printed, roster.into_iter().collect::<BTreeSet<_>>());
}

#[test]
fn the_instance_list_declares_prod_and_the_playground() {
    let inst = instances_of(&repo_root());
    let prod = &inst["prod"];
    let play = &inst["playground"];
    assert_eq!(prod["namespace"], "boss");
    assert_eq!(
        prod["sim"], "false",
        "the sim stays parked on prod (2026-09-05)"
    );
    assert_eq!(play["namespace"], "boss-playground");
    assert_eq!(
        play["sim"], "true",
        "the brewery sim runs on the playground"
    );
    assert_eq!(
        play["tenant_dir"], "examples/brewery",
        "the playground boots the public example from the image"
    );
    assert_eq!(
        prod["tenant_repo"], "david/algedonic-llc",
        "prod boots the company's own tenant from its repo (the flip, 2026-09-16)"
    );
    assert_eq!(play["hostname"], "playground.algedonic.dev");
    assert_eq!(
        inst[""]["source"], "prod",
        "the directory is written for prod"
    );
    // GUEST ACCESS IS PER INSTANCE (backlog 0d2d7daa, 2026-09-16).
    // BOSS_GUEST_ACCESS=1 hands an anonymous visitor a read-only
    // session — right for the public example, wrong for the operating
    // site behind Access. The playground keeps it; the flip (2026-09-16)
    // set `guest = false` here and `"0"` in boss.yaml together.
    assert_eq!(
        play["guest"], "true",
        "anonymous read-only sessions stay on for the public playground"
    );
    assert_eq!(
        prod["guest"], "false",
        "the operating site hands out no anonymous session (the flip)"
    );
}

#[test]
fn the_prod_render_is_byte_identical_to_the_tree() {
    let out = scratch_dir("render-instance-prod").join("out");
    let (rc, _, err) = run(&repo_root(), &["--all", out.to_str().unwrap()]);
    assert_eq!(rc, 0, "--all: {err}");
    let dir = repo_root().join(MANIFESTS);
    let rendered = yaml_names(&out.join("boss"));
    assert_eq!(
        rendered,
        yaml_names(&dir),
        "the prod render carries every manifest — instance AND pipeline — and nothing else"
    );
    for name in rendered {
        let want = std::fs::read(dir.join(&name)).unwrap();
        let got = std::fs::read(out.join("boss").join(&name)).unwrap();
        assert!(
            want == got,
            "{name}: the prod render differs from the tree — the substitutions must be the \
             identity for the source instance, or landing the renderer changes what is live"
        );
    }
    // The stream form says the same thing, file after file — with the
    // source's own parameters, read from instances.toml (the site is
    // the optional sixth; a_tenant_site_per_instance.rs).
    let prod = &instances_of(&repo_root())["prod"];
    let site = prod.get("site").cloned().unwrap_or_default();
    let (rc, stream, err) = run(
        &repo_root(),
        &[
            &prod["namespace"],
            &tenant_of(prod),
            &prod["sim"],
            &prod["hostname"],
            &prod["guest"],
            &site,
        ],
    );
    assert_eq!(rc, 0, "stream render: {err}");
    let boss_yaml = std::fs::read_to_string(dir.join("boss.yaml")).unwrap();
    assert!(
        stream.contains(&boss_yaml),
        "the stream carries boss.yaml verbatim"
    );
}

#[test]
fn the_playground_render_substitutes_the_four_parameters() {
    let out = scratch_dir("render-instance-playground").join("out");
    let (rc, _, err) = run(&repo_root(), &["--all", out.to_str().unwrap()]);
    assert_eq!(rc, 0, "--all: {err}");
    let play = out.join("boss-playground");
    let instance_set: BTreeSet<String> = roster_of(&repo_root())
        .into_iter()
        .filter(|(_, s)| s == "instance")
        .map(|(f, _)| f)
        .collect();
    assert_eq!(
        yaml_names(&play),
        instance_set,
        "the playground gets the instance set and no pipeline object"
    );
    let dir = repo_root().join(MANIFESTS);
    for name in &instance_set {
        let src = std::fs::read_to_string(dir.join(name)).unwrap();
        let got = std::fs::read_to_string(play.join(name)).unwrap();
        for (n, line) in got.lines().enumerate() {
            let t = line.trim_start();
            if t.starts_with('#') {
                continue;
            }
            assert!(
                !t.eq("namespace: boss"),
                "{name}:{}: still in namespace boss",
                n + 1
            );
            assert!(
                !line.contains(".boss.svc.cluster.local"),
                "{name}:{}: still names a prod service by DNS: {line}",
                n + 1
            );
            assert!(
                !t.starts_with("loadBalancerIP:")
                    && !t.starts_with("io.cilium/lb-ipam-ips:")
                    && !t.starts_with("metallb.universe.tf/loadBalancerIPs:"),
                "{name}:{}: a LoadBalancer IP pin survived into the playground — only one \
                 Service on the LAN can hold it: {line}",
                n + 1
            );
        }
        let src_ns = src.matches("\n  namespace: boss\n").count();
        let got_ns = got.matches("\n  namespace: boss-playground\n").count();
        assert_eq!(got_ns, src_ns, "{name}: every object moved namespace");
        let src_dns = src.matches(".boss.svc.cluster.local").count();
        let got_dns = got.matches(".boss-playground.svc.cluster.local").count();
        assert_eq!(
            got_dns, src_dns,
            "{name}: every in-cluster DNS name moved namespace"
        );
    }
    let boss_yaml = std::fs::read_to_string(play.join("boss.yaml")).unwrap();
    assert!(
        boss_yaml.contains("kind: Namespace\nmetadata:\n  name: boss-playground\n"),
        "the Namespace object is the playground's"
    );
    assert!(
        boss_yaml.contains("name: boss\n  namespace: boss-playground\n"),
        "the Deployment keeps its name inside the new namespace"
    );
    assert!(
        boss_yaml.contains("BOSS_SIM_ENABLED, value: \"true\""),
        "the brewery sim runs on the playground"
    );
    assert!(
        !boss_yaml.contains("BOSS_SIM_ENABLED, value: \"false\""),
        "the sim flag was substituted, not duplicated"
    );
    assert!(
        boss_yaml.contains("BOSS_GUEST_ACCESS, value: \"1\""),
        "the playground keeps anonymous read-only sessions (guest = true)"
    );
    let play_tenant = instances_of(&repo_root())["playground"]["tenant_dir"].clone();
    assert!(
        boss_yaml.contains(&format!("BOSS_TENANT_DIR, value: /opt/boss/{play_tenant}")),
        "the tenant directory is rendered under /opt/boss as the image ships it"
    );
    assert!(
        boss_yaml.contains("io.cilium/lb-ipam-ips"),
        "the pin is commented, not erased — the record of what prod holds stays readable"
    );
    // The hostname parameter lands on the gateway's own public URL:
    // since 21c17ebc (2026-09-17) there is no TLS front — Cloudflare
    // terminates TLS at the edge and the tunnel connector proxies each
    // hostname to the instance's gateway Service in plain HTTP — so the
    // one place an instance's hostname appears is BOSS_PUBLIC_URL.
    assert!(
        boss_yaml.contains("BOSS_PUBLIC_URL, value: \"https://playground.algedonic.dev\""),
        "the gateway's public URL is the playground's hostname"
    );
    assert!(
        !boss_yaml.contains("boss.algedonic.dev"),
        "no prod hostname survives into the playground's boss.yaml"
    );
    // The chores open their packets on the playground's own jobs door.
    let chore = std::fs::read_to_string(play.join("boss-audit-integrity.yaml")).unwrap();
    assert!(chore.contains("http://boss-jobs-internal.boss-playground.svc.cluster.local:7900"));
    // A different tenant renders as a different path — the parameter
    // is honoured, not the one value the tree happens to carry today.
    let tree = fixture("other-tenant");
    let other = tree.join("examples/other/tenant.toml");
    std::fs::create_dir_all(other.parent().unwrap()).unwrap();
    write_file(&other, "[tenant]\nname = \"other\"\n");
    let (rc, stream, err) = run(
        &tree,
        &[
            "boss-other",
            "examples/other",
            "true",
            "other.example",
            "false",
        ],
    );
    assert_eq!(rc, 0, "{err}");
    assert!(stream.contains("BOSS_TENANT_DIR, value: /opt/boss/examples/other}"));
    assert!(
        !stream.contains("/opt/boss/tenant}"),
        "the source value was substituted away"
    );
    // guest = false renders the value the gateway reads as "no guest
    // button" (boss-gateway/src/main.rs: guest_access iff == "1") —
    // the line the flip car puts on prod. Substituted, not duplicated.
    assert!(
        stream.contains("BOSS_GUEST_ACCESS, value: \"0\""),
        "guest = false renders BOSS_GUEST_ACCESS \"0\":\n{stream}"
    );
    assert!(!stream.contains("BOSS_GUEST_ACCESS, value: \"1\""));
    // And the delivered mount of a repo-sourced instance (f4f5c387):
    // `tenant` is nowhere in the tree by design.
    let (rc, stream, err) = run(
        &tree,
        &["boss-other", "tenant", "true", "other.example", "true"],
    );
    assert_eq!(rc, 0, "{err}");
    assert!(stream.contains("BOSS_TENANT_DIR, value: /opt/boss/tenant}"));
}

#[test]
fn no_manifest_references_the_boss_tls_secret_or_the_lego_jobs() {
    // Let's Encrypt left the cluster (backlog 21c17ebc; design 4c565f8c,
    // decided 2026-09-16): Cloudflare terminates TLS at the edge and
    // every public hostname reaches an instance's gateway through the
    // tunnel, so the lego DNS-01 Jobs, the Caddy front and the `boss-tls`
    // Secret they existed for are gone. Measured 2026-09-17: the
    // playground was SKIPPED on every converge for want of that Secret
    // (`instances_skipped: boss-playground (secrets absent: boss-tls)`)
    // because boss-tls-front.yaml still mounted it. The converge DERIVES
    // an instance's required Secrets from its rendered manifests, so the
    // skip ends exactly when the last reference does — this pins that no
    // reference comes back.
    let dir = repo_root().join(MANIFESTS);
    for gone in ["boss-tls.yaml", "boss-tls-front.yaml"] {
        assert!(
            !dir.join(gone).exists(),
            "{MANIFESTS}/{gone} is back — Let's Encrypt left the cluster with 21c17ebc"
        );
    }
    let roster = roster_of(&repo_root());
    assert!(
        !roster
            .iter()
            .any(|(f, _)| f == "boss-tls.yaml" || f == "boss-tls-front.yaml"),
        "{ROSTER} still classifies a TLS manifest the directory no longer holds"
    );
    for name in yaml_names(&dir) {
        let text = std::fs::read_to_string(dir.join(&name)).unwrap();
        for (n, line) in text.lines().enumerate() {
            let t = line.trim_start();
            if t.starts_with('#') {
                continue;
            }
            assert!(
                !line.contains("boss-tls") && !line.contains("goacme/lego"),
                "{MANIFESTS}/{name}:{}: references the retired TLS machinery — a `boss-tls` \
                 reference makes the converge skip every instance whose namespace lacks a \
                 Secret nothing mints: {line}",
                n + 1
            );
        }
    }
}

#[test]
fn the_renderer_refuses_bad_parameters() {
    let tree = repo_root();
    let prod_tenant = tenant_of(&instances_of(&tree)["prod"]);
    let tenant = prod_tenant.as_str();
    let cases: &[(&[&str], &str)] = &[
        (&["prod", tenant, "false", "h.example", "true"], "namespace"),
        (
            &["boss-dev", tenant, "false", "h.example", "true"],
            "namespace",
        ),
        (&["Boss", tenant, "false", "h.example", "true"], "namespace"),
        (
            &["boss_x", tenant, "false", "h.example", "true"],
            "namespace",
        ),
        (
            &["boss-x", "../etc/passwd", "false", "h.example", "true"],
            "tenant",
        ),
        (
            &["boss-x", "infra/cluster", "false", "h.example", "true"],
            "tenant",
        ),
        (
            &["boss-x", "examples/nope", "false", "h.example", "true"],
            "tenant",
        ),
        (&["boss-x", tenant, "yes", "h.example", "true"], "sim"),
        (&["boss-x", tenant, "true", "bad host", "true"], "hostname"),
        (&["boss-x", tenant, "true", "h.example", "1"], "guest"),
        (&["boss-x", tenant, "true", "h.example"], "usage"),
        (&["boss-x", tenant, "true"], "usage"),
    ];
    for (args, what) in cases {
        let (rc, out, err) = run(&tree, args);
        assert_eq!(rc, REFUSED, "{args:?} must be refused, got rc {rc}: {err}");
        assert!(
            out.is_empty(),
            "{args:?}: a refusal renders nothing, got: {out}"
        );
        assert!(
            err.to_lowercase().contains(what),
            "{args:?}: the refusal names what was wrong ({what}): {err}"
        );
    }
}

#[test]
fn the_renderer_refuses_a_manifest_the_roster_does_not_classify() {
    // A file in the directory the roster has never heard of.
    let tree = fixture("unclassified");
    write_file(
        &tree.join(MANIFESTS).join("boss-extra.yaml"),
        "kind: Service\nmetadata:\n  name: extra\n  namespace: boss\n",
    );
    let out = tree.join("out");
    let (rc, _, err) = run(&tree, &["--all", out.to_str().unwrap()]);
    assert_eq!(
        rc, REFUSED,
        "an unclassified manifest must refuse the render: {err}"
    );
    assert!(
        err.contains("boss-extra.yaml"),
        "the refusal names the file: {err}"
    );
    assert!(
        !out.exists() || yaml_names(&out.join("boss")).is_empty(),
        "nothing was rendered"
    );

    // A roster entry with no file behind it.
    let tree = fixture("phantom");
    std::fs::remove_file(tree.join(MANIFESTS).join("boss-files-gc.yaml")).unwrap();
    let (rc, _, err) = run(&tree, &["--roster"]);
    assert_eq!(rc, REFUSED, "a phantom roster entry must refuse: {err}");
    assert!(
        err.contains("boss-files-gc.yaml"),
        "the refusal names the entry: {err}"
    );

    // A file in both sets.
    let tree = fixture("both");
    let roster = std::fs::read_to_string(tree.join(ROSTER)).unwrap();
    write_file(
        &tree.join(ROSTER),
        &format!("{roster}\nboss-files-gc.yaml pipeline\n"),
    );
    let (rc, _, err) = run(&tree, &["--roster"]);
    assert_eq!(rc, REFUSED, "a manifest in both sets must refuse: {err}");
    assert!(
        err.contains("boss-files-gc.yaml"),
        "the refusal names the entry: {err}"
    );
}

#[test]
fn the_renderer_refuses_an_instance_that_does_not_say_whether_guests_may_read() {
    // A missing `guest` must not read as "guest access on": an
    // instance that inherited the manifest's "1" by silence would hand
    // anonymous visitors the operating company's read-only view.
    let tree = fixture("no-guest");
    let toml = std::fs::read_to_string(tree.join(INSTANCES)).unwrap();
    let (head, play) = toml.split_once("[playground]").unwrap();
    let play = play.replacen("guest = true\n", "", 1);
    assert_ne!(
        play, toml,
        "the fixture removes the playground's guest line"
    );
    write_file(&tree.join(INSTANCES), &format!("{head}[playground]{play}"));
    let out = tree.join("out");
    let (rc, _, err) = run(&tree, &["--all", out.to_str().unwrap()]);
    assert_eq!(
        rc, REFUSED,
        "an instance with no guest line must refuse: {err}"
    );
    assert!(
        err.contains("[playground]") && err.contains("guest"),
        "the refusal names the instance and the parameter: {err}"
    );
}

#[test]
fn the_renderer_refuses_a_source_value_the_files_do_not_carry() {
    // The prod entry says www while the files still say boss: every
    // other instance's hostname substitution would silently match
    // nothing. That is a wrong target answering instead of erroring.
    let tree = fixture("source-drift");
    let toml = std::fs::read_to_string(tree.join(INSTANCES)).unwrap();
    let drifted = toml.replacen("boss.algedonic.dev", "www.algedonic.dev", 1);
    assert_ne!(toml, drifted, "the fixture perturbs prod's hostname");
    write_file(&tree.join(INSTANCES), &drifted);
    let out = tree.join("out");
    let (rc, _, err) = run(&tree, &["--all", out.to_str().unwrap()]);
    assert_eq!(
        rc, REFUSED,
        "a source value absent from the files must refuse: {err}"
    );
    assert!(
        err.contains("www.algedonic.dev") && err.to_lowercase().contains("hostname"),
        "the refusal names the value and the parameter: {err}"
    );
}
