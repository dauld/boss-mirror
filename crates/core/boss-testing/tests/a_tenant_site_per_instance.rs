//! An instance may declare a SITE — a second public hostname, answered
//! by the instance's gateway from the tenant repository's `site/`
//! directory — and every reader of the instance list carries it: the
//! manifest render, the tunnel ingress, the converge's staging and the
//! orphan sweep's derived exemptions. The renderer, the tunnel render
//! and the deploy lib are RUN against the tree's own files and fixture
//! trees; nothing here touches a cluster or the forge.
//!
//! WHY (design b64c4377, decided by David 2026-09-17; backlog
//! c8f6b233). The operating company's website is company data — static
//! pages in `site/` of the tenant repo david/algedonic-llc — and it is
//! published the way the seeds are: the converge stages the directory
//! into a ConfigMap (`boss-site`, beside `boss-tenant`, the same 1 MiB
//! bound) and the gateway serves it under the hostname the instance
//! declares, with no session (boss-gateway site.rs). So the ONE
//! declaration is `site = "www.algedonic.dev"` on prod in
//! infra/cluster/instances.toml, and this file holds every derived copy
//! to it (CLAUDE.md §9a): BOSS_SITE_HOST on the gateway, the tunnel rule
//! `www → boss-gateway.boss`, the ConfigMap the converge delivers and
//! the sweep expects.
//!
//! HOW AN INSTANCE WITHOUT A SITE RENDERS. The renderer is a line
//! substitution over the source instance's manifests, so the site is
//! not a block that appears or vanishes: boss.yaml carries the env and
//! an `optional` ConfigMap mount for every instance, and an instance
//! that declares no site renders `BOSS_SITE_HOST` as `""` — the
//! gateway's spelling of "no site" (site.rs `Site::from_values`) — over
//! an empty mount. The boss-tenant volume set the idiom.

use boss_testing::{repo_root, scratch_dir, write_file};
use std::path::{Path, PathBuf};
use std::process::Command;

const LIB: &str = "infra/forge/cluster-deploy-lib.sh";
const RUNNER: &str = "infra/forge/cluster-deploy-runner.sh";
const RENDERER: &str = "infra/cluster/render-instance.sh";
const TUNNEL: &str = "infra/cluster/render-tunnel-config.sh";
const SWEEP: &str = "infra/cluster/undeclared-objects.sh";
const INSTANCES: &str = "infra/cluster/instances.toml";
const BOSS_YAML: &str = "infra/cluster/manifests/boss.yaml";
const TUNNEL_CONFIG: &str = "infra/cluster/manifests/cloudflared-config.yaml";
const REFUSED: i32 = 2;
const SITE: &str = "www.algedonic.dev";
const SITE_DIR: &str = "/opt/boss/site";

fn run_script(script: &str, tree: &Path, args: &[&str]) -> (i32, String, String) {
    let out = Command::new("bash")
        .arg(repo_root().join(script))
        .args(args)
        .env("BOSS_CLUSTER_TREE", tree)
        .output()
        .expect("bash runs the script");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

fn render(tree: &Path, args: &[&str]) -> (i32, String, String) {
    run_script(RENDERER, tree, args)
}

fn tunnel(tree: &Path, args: &[&str]) -> (i32, String, String) {
    run_script(TUNNEL, tree, args)
}

/// The repository's own manifests, roster, instance list, tunnel
/// origins and tenant directories copied into scratch, so a case can
/// perturb the instance list alone (a_tenant_source_per_instance.rs).
fn fixture_tree(case: &str) -> PathBuf {
    let tree = scratch_dir(&format!("tenant-site-tree-{case}")).join("tree");
    let manifests = "infra/cluster/manifests";
    std::fs::create_dir_all(tree.join(manifests)).unwrap();
    for e in std::fs::read_dir(repo_root().join(manifests)).unwrap() {
        let e = e.unwrap();
        std::fs::copy(e.path(), tree.join(manifests).join(e.file_name())).unwrap();
    }
    for f in [
        "infra/cluster/instance-manifests.txt",
        "infra/cluster/tunnel-origins.toml",
        INSTANCES,
    ] {
        std::fs::copy(repo_root().join(f), tree.join(f)).unwrap();
    }
    let toml = std::fs::read_to_string(repo_root().join(INSTANCES)).unwrap();
    for line in toml.lines() {
        if let Some(d) = line.trim().strip_prefix("tenant_dir = ") {
            let d = d.trim_matches('"');
            std::fs::create_dir_all(tree.join(d).join("seeds")).unwrap();
            std::fs::copy(
                repo_root().join(d).join("seeds/tenant.toml"),
                tree.join(d).join("seeds/tenant.toml"),
            )
            .unwrap();
        }
    }
    tree
}

fn instances_toml(tree: &Path) -> String {
    std::fs::read_to_string(tree.join(INSTANCES)).unwrap()
}

/// The instance list with prod's `site` line replaced (or removed,
/// with an empty replacement).
fn with_prod_site(tree: &Path, line: &str) {
    let toml = instances_toml(tree);
    let old = format!("site = \"{SITE}\"\n");
    assert!(toml.contains(&old), "prod declares the site");
    let new = if line.is_empty() {
        String::new()
    } else {
        format!("{line}\n")
    };
    write_file(&tree.join(INSTANCES), &toml.replacen(&old, &new, 1));
}

fn rows(out: &str) -> Vec<Vec<String>> {
    out.lines()
        .map(|l| l.split('\t').map(str::to_string).collect())
        .collect()
}

// ---- the tree, as shipped --------------------------------------------

#[test]
fn prod_declares_the_site_and_every_derived_copy_follows_it() {
    let toml = instances_toml(&repo_root());
    let (_, prod) = toml.split_once("[prod]").expect("the prod section");
    let prod = prod.split("[playground]").next().unwrap();
    assert!(
        prod.contains(&format!("site = \"{SITE}\"")),
        "prod declares its site: the one declaration every copy below derives from"
    );
    assert!(
        !toml
            .split("[playground]")
            .nth(1)
            .unwrap()
            .contains("site = "),
        "the playground declares no site"
    );

    // The instance list: a ninth column, empty for an instance without
    // a site, kept in place for the runner's readers (instance_rows).
    let (rc, out, err) = render(&repo_root(), &["--instances"]);
    assert_eq!(rc, 0, "{err}");
    let rows = rows(&out);
    for name in ["prod", "playground"] {
        let row = rows.iter().find(|r| r[0] == name).expect(name);
        assert_eq!(
            row.len(),
            9,
            "name, namespace, tenant, sim, hostname, shares-with, tenant-repo, tenant-ref, site: {row:?}"
        );
    }
    let prod_row = rows.iter().find(|r| r[0] == "prod").unwrap();
    let play_row = rows.iter().find(|r| r[0] == "playground").unwrap();
    assert_eq!(prod_row[8], SITE, "the ninth column is the site hostname");
    assert_eq!(play_row[8], "", "no site, an empty column");

    // The gateway's env and the delivered mount, as the source instance
    // (prod) is written.
    let boss = std::fs::read_to_string(repo_root().join(BOSS_YAML)).unwrap();
    assert!(
        boss.contains(&format!("- {{name: BOSS_SITE_HOST, value: \"{SITE}\"}}")),
        "the boss container declares the site host (the source's value)"
    );
    assert!(
        boss.contains(&format!("- {{name: BOSS_SITE_DIR, value: {SITE_DIR}}}")),
        "the boss container declares the site directory"
    );
    assert!(
        boss.contains(&format!("mountPath: {SITE_DIR}, readOnly: true")),
        "the delivered site mounts at BOSS_SITE_DIR, read-only"
    );
    let vol = boss
        .find("- name: boss-site\n")
        .expect("the boss-site volume is declared");
    assert!(
        boss[vol..vol + 200].contains("name: boss-site\n")
            && boss[vol..vol + 200].contains("optional: true"),
        "the ConfigMap is optional: an instance without a site, or one whose tenant has no \
         site/ yet, must still boot"
    );

    // The tunnel: the site routes to the SAME gateway as the instance's
    // hostname, directly after it, and the committed config is the
    // render (the equality test the tunnel car left).
    let (rc, out, err) = tunnel(&repo_root(), &[]);
    assert_eq!(rc, 0, "{err}");
    let prod_rule = out
        .find("- hostname: boss.algedonic.dev\n")
        .expect("prod's rule");
    let site_rule = out
        .find(&format!("- hostname: {SITE}\n"))
        .expect("the site's rule");
    assert!(
        site_rule > prod_rule && out[prod_rule..site_rule].matches("- hostname:").count() == 1,
        "the site rule follows its instance's rule: {out}"
    );
    assert!(
        out[site_rule..]
            .lines()
            .nth(1)
            .unwrap()
            .contains("service: http://boss-gateway.boss.svc.cluster.local:80"),
        "the site is served by prod's gateway: {out}"
    );
    assert_eq!(
        out,
        std::fs::read_to_string(repo_root().join(TUNNEL_CONFIG)).unwrap(),
        "the committed connector config is the render — run render-tunnel-config.sh --write"
    );
    let (rc, summary, err) = tunnel(&repo_root(), &["--summary"]);
    assert_eq!(rc, 0, "{err}");
    assert!(
        summary.contains(&format!("boss.algedonic.dev → boss; {SITE} → boss (site);")),
        "the packet's tunnel_ingress line names the site beside its instance: {summary}"
    );

    // The orphan sweep expects the delivered ConfigMap, derived like
    // boss-tenant: present when the converge staged a site, never stale
    // when absent.
    let (rc, derived, err) = run_script(SWEEP, &repo_root(), &["--exemptions-derived"]);
    assert_eq!(rc, 0, "{err}");
    let derived: Vec<&str> = derived.lines().collect();
    assert!(
        derived.contains(&"ConfigMap/boss/boss-site"),
        "the sweep derives prod's boss-site ConfigMap from the instance list: {derived:?}"
    );
    assert!(
        !derived
            .iter()
            .any(|l| l.contains("boss-playground/boss-site")),
        "no site, no derived ConfigMap: {derived:?}"
    );
}

// ---- the render, per instance ------------------------------------------

#[test]
fn an_instance_without_a_site_renders_an_empty_host_over_the_optional_mount() {
    let out_dir = scratch_dir("tenant-site-render").join("out");
    let (rc, _, err) = render(&repo_root(), &["--all", out_dir.to_str().unwrap()]);
    assert_eq!(rc, 0, "{err}");
    let play = std::fs::read_to_string(out_dir.join("boss-playground/boss.yaml")).unwrap();
    assert!(
        play.contains("- {name: BOSS_SITE_HOST, value: \"\"}"),
        "no site: the host renders empty, the gateway's inert spelling: {play}"
    );
    assert!(
        !play
            .lines()
            .filter(|l| !l.trim_start().starts_with('#'))
            .any(|l| l.contains(SITE)),
        "prod's site hostname survives into no other instance's manifest"
    );
    assert!(
        play.contains(&format!("- {{name: BOSS_SITE_DIR, value: {SITE_DIR}}}"))
            && play.contains("- name: boss-site\n"),
        "the directory and the optional mount stay: the line substitution touches the host only"
    );
    let prod = std::fs::read_to_string(out_dir.join("boss/boss.yaml")).unwrap();
    assert_eq!(
        prod,
        std::fs::read_to_string(repo_root().join(BOSS_YAML)).unwrap(),
        "the source instance's render is the identity"
    );

    // The positional form takes the site as its sixth argument, and
    // renders none without it.
    let tree = fixture_tree("positional");
    let (rc, stream, err) = render(
        &tree,
        &[
            "boss-other",
            "tenant",
            "true",
            "other.example",
            "false",
            "www.other.example",
        ],
    );
    assert_eq!(rc, 0, "{err}");
    assert!(
        stream.contains("- {name: BOSS_SITE_HOST, value: \"www.other.example\"}"),
        "{stream}"
    );
    assert!(
        !stream.contains(SITE),
        "the source's site was substituted away"
    );
    let (rc, stream, err) = render(
        &tree,
        &["boss-other", "tenant", "true", "other.example", "false"],
    );
    assert_eq!(rc, 0, "{err}");
    assert!(
        stream.contains("- {name: BOSS_SITE_HOST, value: \"\"}"),
        "five arguments: no site: {stream}"
    );
    // With --out-dir too.
    let out = tree.join("out-positional");
    let (rc, _, err) = render(
        &tree,
        &[
            "boss-other",
            "tenant",
            "true",
            "other.example",
            "false",
            "www.other.example",
            "--out-dir",
            out.to_str().unwrap(),
        ],
    );
    assert_eq!(rc, 0, "{err}");
    assert!(
        std::fs::read_to_string(out.join("boss.yaml"))
            .unwrap()
            .contains("BOSS_SITE_HOST, value: \"www.other.example\""),
        "the out-dir form carries the site"
    );
}

#[test]
fn a_site_that_cannot_be_served_is_refused_by_name() {
    // The site equals the instance's own hostname: two names for one
    // route, and the gateway would answer the site for every request.
    let tree = fixture_tree("site-is-hostname");
    with_prod_site(&tree, "site = \"boss.algedonic.dev\"");
    let (rc, _, err) = render(&tree, &["--instances"]);
    assert_eq!(rc, REFUSED, "{err}");
    assert!(
        err.contains("site") && err.contains("boss.algedonic.dev") && err.contains("[prod]"),
        "the refusal names the key, the value and the instance: {err}"
    );

    // The site equals another instance's hostname: the tunnel would
    // route one name to two gateways.
    let tree = fixture_tree("site-is-other-hostname");
    with_prod_site(&tree, "site = \"playground.algedonic.dev\"");
    let (rc, _, err) = render(&tree, &["--instances"]);
    assert_eq!(rc, REFUSED, "{err}");
    assert!(
        err.contains("site") && err.contains("playground.algedonic.dev"),
        "{err}"
    );

    // Not a hostname.
    let tree = fixture_tree("site-not-a-hostname");
    with_prod_site(&tree, "site = \"Not A Host\"");
    let (rc, _, err) = render(&tree, &["--instances"]);
    assert_eq!(rc, REFUSED, "{err}");
    assert!(err.contains("site") && err.contains("Not A Host"), "{err}");

    // A site on an image-sourced instance: the converge stages a site
    // from a tenant CHECKOUT, and an image-sourced tenant has none.
    let tree = fixture_tree("site-on-image-tenant");
    let toml = instances_toml(&tree);
    write_file(
        &tree.join(INSTANCES),
        &toml.replacen(
            "hostname = \"playground.algedonic.dev\"\n",
            "hostname = \"playground.algedonic.dev\"\nsite = \"www.playground.example\"\n",
            1,
        ),
    );
    let (rc, _, err) = render(&tree, &["--instances"]);
    assert_eq!(rc, REFUSED, "{err}");
    assert!(
        err.contains("site") && err.contains("[playground]") && err.contains("tenant_repo"),
        "the refusal says a site needs a repo-sourced tenant: {err}"
    );

    // The source's site drifted from the files: prod says one name,
    // boss.yaml's BOSS_SITE_HOST says another — every other instance's
    // substitution would then match nothing.
    let tree = fixture_tree("site-drift");
    with_prod_site(&tree, "site = \"www.other.example\"");
    let (rc, _, err) = render(&tree, &["--all", tree.join("out").to_str().unwrap()]);
    assert_eq!(rc, REFUSED, "{err}");
    assert!(
        err.contains("www.other.example") && err.contains("BOSS_SITE_HOST"),
        "the refusal names the value and the manifest line: {err}"
    );
    assert!(!tree.join("out").exists(), "nothing rendered");

    // And the source declaring NO site while the files carry one is the
    // same drift, the other way.
    let tree = fixture_tree("site-removed");
    with_prod_site(&tree, "");
    let (rc, _, err) = render(&tree, &["--all", tree.join("out").to_str().unwrap()]);
    assert_eq!(rc, REFUSED, "{err}");
    assert!(err.contains("BOSS_SITE_HOST"), "{err}");

    // A source without a site whose files say `""` renders another
    // instance's site onto that line; the same tree with the line
    // missing is refused naming the instance whose site has nowhere to
    // land (a substitution that matches nothing answers instead of
    // erroring).
    let tree = fixture_tree("site-on-other-instance");
    with_prod_site(&tree, "");
    let toml = instances_toml(&tree);
    let (head, play) = toml.split_once("[playground]").unwrap();
    let play = play
        .replacen(
            "tenant_dir = \"examples/brewery\"",
            "tenant_repo = \"david/tenant-fixture\"\ntenant_ref = \"main\"",
            1,
        )
        .replacen(
            "hostname = \"playground.algedonic.dev\"\n",
            "hostname = \"playground.algedonic.dev\"\nsite = \"www.playground.example\"\n",
            1,
        );
    write_file(&tree.join(INSTANCES), &format!("{head}[playground]{play}"));
    let yaml_path = tree.join(BOSS_YAML);
    let yaml = std::fs::read_to_string(&yaml_path).unwrap();
    let blank = format!("- {{name: BOSS_SITE_HOST, value: \"{SITE}\"}}");
    assert!(yaml.contains(&blank));
    write_file(
        &yaml_path,
        &yaml.replacen(&blank, "- {name: BOSS_SITE_HOST, value: \"\"}", 1),
    );
    let out = tree.join("out-other");
    let (rc, _, err) = render(&tree, &["--all", out.to_str().unwrap()]);
    assert_eq!(rc, 0, "{err}");
    assert!(
        std::fs::read_to_string(out.join("boss-playground/boss.yaml"))
            .unwrap()
            .contains("- {name: BOSS_SITE_HOST, value: \"www.playground.example\"}"),
        "the playground's site lands on the blank line"
    );
    let yaml = std::fs::read_to_string(&yaml_path).unwrap();
    write_file(
        &yaml_path,
        &yaml.replacen("- {name: BOSS_SITE_HOST, value: \"\"}\n", "", 1),
    );
    let (rc, _, err) = render(&tree, &["--all", tree.join("out-none").to_str().unwrap()]);
    assert_eq!(rc, REFUSED, "{err}");
    assert!(
        err.contains("[playground]") && err.contains("BOSS_SITE_HOST"),
        "{err}"
    );
}

// ---- the tunnel ----------------------------------------------------------

#[test]
fn the_tunnel_routes_the_site_to_its_instances_gateway_and_follows_a_skip() {
    // A skipped instance's hostname is served by the source's gateway
    // (40d46042); its site follows the same route, so the packet line
    // says where the name actually goes.
    let tree = fixture_tree("tunnel-skip");
    let toml = instances_toml(&tree);
    let (head, play) = toml.split_once("[playground]").unwrap();
    let play = play
        .replacen(
            "tenant_dir = \"examples/brewery\"",
            "tenant_repo = \"david/tenant-fixture\"\ntenant_ref = \"main\"",
            1,
        )
        .replacen(
            "hostname = \"playground.algedonic.dev\"\n",
            "hostname = \"playground.algedonic.dev\"\nsite = \"www.playground.example\"\n",
            1,
        );
    write_file(&tree.join(INSTANCES), &format!("{head}[playground]{play}"));
    let out = Command::new("bash")
        .arg(repo_root().join(TUNNEL))
        .arg("--summary")
        .env("BOSS_CLUSTER_TREE", &tree)
        .env(
            "BOSS_INSTANCES_SKIPPED",
            "boss-playground (secrets absent: boss-oidc)",
        )
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let summary = String::from_utf8_lossy(&out.stdout);
    assert!(
        summary.contains(
            "playground.algedonic.dev → boss (boss-playground skipped: secrets absent); \
             www.playground.example → boss (boss-playground skipped: secrets absent; site)"
        ),
        "{summary}"
    );
    let (rc, config, err) = tunnel(&tree, &[]);
    assert_eq!(rc, 0, "{err}");
    let site_rule = config
        .find("- hostname: www.playground.example\n")
        .expect("the playground's site rule");
    assert!(
        config[site_rule..]
            .lines()
            .nth(1)
            .unwrap()
            .contains("boss-gateway.boss-playground.svc.cluster.local"),
        "applied: the site routes to its own instance's gateway: {config}"
    );

    // A site that is a declared origin's hostname is one route too
    // many, refused by name.
    let tree = fixture_tree("tunnel-site-is-origin");
    with_prod_site(&tree, "site = \"id.algedonic.dev\"");
    let (rc, _, err) = tunnel(&tree, &[]);
    assert_eq!(rc, REFUSED, "{err}");
    assert!(err.contains("id.algedonic.dev"), "{err}");
}

// ---- the converge ---------------------------------------------------------

#[test]
fn the_site_directory_is_staged_flat_beside_the_tenant_and_bounded_like_it() {
    let root = scratch_dir("tenant-site-stage");
    let src = root.join("checkout");
    std::fs::create_dir_all(src.join("site/css")).unwrap();
    std::fs::create_dir_all(src.join("seeds")).unwrap();
    write_file(&src.join("tenant.toml"), "[meta]\ntenant_id = \"x\"\n");
    write_file(&src.join("site/index.html"), "<p>hello</p>\n");
    write_file(&src.join("site/site.css"), "body{}\n");
    write_file(&src.join("site/css/nested.css"), "p{}\n");
    let stage = root.join("stage");
    let run = |body: &str| {
        let out = Command::new("bash")
            .arg("-c")
            .arg(format!(". '{}'\n{body}\n", repo_root().join(LIB).display()))
            .env("SRC", &src)
            .env("STAGE", &stage)
            .output()
            .unwrap();
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).trim().to_string(),
            String::from_utf8_lossy(&out.stderr).to_string(),
        )
    };
    let (rc, count, err) = run(r#"site_stage "$SRC" "$STAGE""#);
    assert_eq!(rc, 0, "{err}");
    assert_eq!(count, "2", "the file count is printed: the packet's line");
    let mut names: Vec<String> = std::fs::read_dir(&stage)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
        .collect();
    names.sort();
    assert_eq!(
        names,
        ["index.html", "site.css"],
        "every file directly under site/; no subdirectory (a ConfigMap is flat)"
    );

    // No site/ at all: nothing staged, an empty stage, count 0, rc 0 —
    // the tenant simply has no site yet, which is not a refusal.
    std::fs::remove_dir_all(src.join("site")).unwrap();
    std::fs::remove_dir_all(&stage).unwrap();
    let (rc, count, err) = run(r#"site_stage "$SRC" "$STAGE""#);
    assert_eq!(rc, 0, "{err}");
    assert_eq!(count, "0");
    assert!(
        stage.is_dir() && std::fs::read_dir(&stage).unwrap().next().is_none(),
        "an empty stage: the ConfigMap applied from it is empty, and the gateway answers 404"
    );

    // Over a ConfigMap's size: refused with the number, nothing staged.
    std::fs::create_dir_all(src.join("site")).unwrap();
    write_file(&src.join("site/big.html"), &"x".repeat(1_100_000));
    std::fs::remove_dir_all(&stage).unwrap();
    let (rc, _, err) = run(r#"site_stage "$SRC" "$STAGE""#);
    assert_eq!(rc, 2, "a site a ConfigMap cannot hold is refused: {err}");
    assert!(err.contains("MiB") || err.contains("bytes"), "{err}");
    assert!(!stage.exists(), "nothing staged");
}

#[test]
fn the_runner_delivers_the_site_beside_the_tenant_and_reads_the_column() {
    let src = std::fs::read_to_string(repo_root().join(RUNNER)).unwrap();
    let ct = src
        .find("converge_tenant() {")
        .expect("converge_tenant is defined");
    let body = &src[ct..ct + src[ct..].find("\n}\n").unwrap()];
    assert!(
        body.contains("site_stage") && body.contains("create configmap boss-site"),
        "the tenant's site/ is staged and applied as ConfigMap boss-site in the same function \
         that delivers boss-tenant: {body}"
    );
    assert!(
        body.contains("run_summary_field site_source"),
        "what was staged rides the packet as `site_source`: {body}"
    );
    // Every instance loop reads the ninth column, so an empty site
    // column never bleeds into `tref` (instance_rows keeps it).
    assert_eq!(
        src.matches("read -r iname ins_ns tdir _s _h _share trepo tref site")
            .count()
            + src
                .matches("read -r iname ins_ns tdir _s _h share_ns trepo tref site")
                .count(),
        2,
        "both tenant-reading loops name the site column"
    );
    assert!(
        src.contains("converge_tenant \"$SOURCE_NAME\" \"$SOURCE_NS\" \"$SOURCE_TENANT_REPO\" \"$SOURCE_TENANT_REF\" \"$SOURCE_SITE\"")
            && src.contains("converge_tenant \"$iname\" \"$ins_ns\" \"$trepo\" \"$tref\" \"$site\""),
        "the site is handed to converge_tenant for the source and for every other instance"
    );
    let lib = std::fs::read_to_string(repo_root().join(LIB)).unwrap();
    assert!(
        lib.contains("read -r iname ins_ns _t _s _h _share trepo tref _site"),
        "the no-op tick's reader names the column too"
    );
}

// ---- the orphan sweep -----------------------------------------------------

#[test]
fn the_delivered_site_is_exempt_by_derivation_only_where_a_site_is_declared() {
    let tree = scratch_dir("tenant-site-sweep").join("tree");
    std::fs::create_dir_all(tree.join("infra/cluster/manifests")).unwrap();
    write_file(
        &tree.join(INSTANCES),
        "source = \"prod\"\n\n[prod]\nnamespace = \"boss\"\ntenant_repo = \"david/algedonic-llc\"\ntenant_ref = \"main\"\nsim = false\nhostname = \"h.example\"\nsite = \"www.h.example\"\nguest = false\n\n[play]\nnamespace = \"boss-play\"\ntenant_repo = \"david/play\"\ntenant_ref = \"main\"\nsim = true\nhostname = \"p.example\"\nguest = true\n",
    );
    let (rc, derived, err) = run_script(SWEEP, &tree, &["--exemptions-derived"]);
    assert_eq!(rc, 0, "{err}");
    let mut lines: Vec<&str> = derived.lines().collect();
    lines.sort();
    assert_eq!(
        lines,
        [
            "ConfigMap/boss-play/boss-tenant",
            "ConfigMap/boss/boss-site",
            "ConfigMap/boss/boss-tenant",
        ],
        "boss-tenant for every repo-sourced instance; boss-site only where a site is declared"
    );
}
