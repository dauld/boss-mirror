//! Every instance names its TENANT SOURCE, and the pod reads a tenant
//! DIRECTORY. The renderer, the deploy lib and the launcher are RUN —
//! against the tree's own files, fixture trees, a fixture forge and a
//! stub `kubectl` — so every verdict below is one they actually reached.
//! Nothing here touches a cluster or the forge.
//!
//! WHY (backlog f4f5c387, car 2 of fcc1d57b; David 2026-09-16 'Let's do
//! it' — design ffc83387: prod is the current instance, the playground
//! a second namespace). Measured before this car:
//! infra/cluster/manifests/boss.yaml set BOSS_TENANT_MANIFEST_TOML to
//! /opt/boss/examples/brewery/seeds/tenant.toml, instances.toml's
//! `tenant` was that repo-relative path, and the image bakes examples/
//! in — so a tenant that is NOT in the product tree (david/algedonic-llc,
//! private on the forge) had no way into a pod at all. Now:
//!
//!   * the boss container reads BOSS_TENANT_DIR — a directory holding
//!     tenant.toml or seeds/tenant.toml, and seeds/ — and the launcher
//!     derives the two paths the N-1 readers take from it, once:
//!     BOSS_TENANT_MANIFEST_TOML (the gateway) and BOSS_SIM_SEEDS_DIR
//!     (the seed scripts and the engine);
//!   * instances.toml names a tenant SOURCE per instance: `tenant_dir`
//!     (a directory in the product checkout, served from the image at
//!     /opt/boss/<dir>) or `tenant_repo` + `tenant_ref` (a forge repo the
//!     converge checks out beside the product and delivers as the
//!     `boss-tenant` ConfigMap, mounted at /opt/boss/tenant/seeds — the
//!     step-plugins ConfigMap's mechanism);
//!   * the converge MEASURES whether its own credential can read the
//!     tenant repo (`git ls-remote`, the URL derived from the checkout's
//!     `forgejo` remote — never a token or URL of its own) and, if not,
//!     skips that instance whole, naming `tenant_source: unreadable` on
//!     the packet, the instance-secrets skip shape — so the first converge
//!     answers whether the runner's token needs a scope from David (a root
//!     ceremony) instead of a person guessing.
//!
//! Prod stays on examples/brewery in this car; the flip is David's
//! decision moment and the file's comment names the lines it changes.
//!
//! What each case pins: the tree's instance list and boss.yaml carry the
//! new shape and not the old; a repo-sourced instance renders the
//! delivered mount and each malformed source is refused by name; the
//! tenant URL is derived from the checkout's own remote and readability
//! is measured, never assumed, with the URL redacted from every message;
//! the tenant directory is staged flat for ONE ConfigMap; the runner
//! measures before it provisions and delivers before it applies, and the
//! no-op tick's skipped set counts an undelivered tenant; and the launcher
//! derives both paths, lets an explicit value win, and refuses a directory
//! with no manifest.

use boss_testing::{repo_root, scratch_dir, write_exec, write_file};
use std::path::{Path, PathBuf};
use std::process::Command;

const LIB: &str = "infra/forge/cluster-deploy-lib.sh";
const RUNNER: &str = "infra/forge/cluster-deploy-runner.sh";
const RENDERER: &str = "infra/cluster/render-instance.sh";
const INSTANCES: &str = "infra/cluster/instances.toml";
const BOSS_YAML: &str = "infra/cluster/manifests/boss.yaml";
const LAUNCHER: &str = "infra/oss-quickstart/services-launcher.sh";
const SKIPPED_LIB: &str = "infra/cluster/instances-skipped.lib.sh";
const REFUSED: i32 = 2;

fn run_renderer(tree: &Path, args: &[&str]) -> (i32, String, String) {
    let out = Command::new("bash")
        .arg(repo_root().join(RENDERER))
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

/// The repository's own manifests, roster, instance list and tenant
/// directories copied into scratch — the idiom render_instance_sh.rs
/// uses — so a case can perturb the instance list alone.
fn fixture_tree(case: &str) -> PathBuf {
    let tree = scratch_dir(&format!("tenant-source-tree-{case}")).join("tree");
    let manifests = "infra/cluster/manifests";
    std::fs::create_dir_all(tree.join(manifests)).unwrap();
    for e in std::fs::read_dir(repo_root().join(manifests)).unwrap() {
        let e = e.unwrap();
        std::fs::copy(e.path(), tree.join(manifests).join(e.file_name())).unwrap();
    }
    for f in ["infra/cluster/instance-manifests.txt", INSTANCES] {
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

/// The playground flipped to a repo source: its `tenant_dir` line
/// replaced by `tenant_repo` + `tenant_ref`.
fn playground_on_a_repo(tree: &Path) {
    let toml = instances_toml(tree);
    let (head, play) = toml
        .split_once("[playground]")
        .expect("the playground section");
    let play = play.replacen(
        "tenant_dir = \"examples/brewery\"",
        "tenant_repo = \"david/tenant-fixture\"\ntenant_ref = \"main\"",
        1,
    );
    assert!(
        play.contains("tenant_repo"),
        "the fixture flipped the playground"
    );
    write_file(&tree.join(INSTANCES), &format!("{head}[playground]{play}"));
}

#[test]
fn the_tree_names_a_tenant_directory_per_instance_and_the_pod_reads_it() {
    let toml = instances_toml(&repo_root());
    for line in toml.lines().map(str::trim) {
        assert!(
            !line.starts_with("tenant = "),
            "instances.toml still declares the retired `tenant` (manifest path) key: {line}"
        );
    }
    // THE FLIP (2026-09-16, David: "prep the flip car"): prod reads the
    // company's own tenant from its repo; the playground keeps the
    // brewery from the image. Before the flip both read the image.
    assert_eq!(
        toml.matches("tenant_dir = \"examples/brewery\"").count(),
        1,
        "the playground reads the brewery from the image"
    );
    assert!(
        toml.contains("tenant_repo = \"david/algedonic-llc\"")
            && toml.contains("tenant_ref = \"main\""),
        "prod reads Algedonic, LLC from its repo at main"
    );
    let (rc, out, err) = run_renderer(&repo_root(), &["--instances"]);
    assert_eq!(rc, 0, "{err}");
    let rows: Vec<Vec<&str>> = out.lines().map(|l| l.split('\t').collect()).collect();
    for name in ["prod", "playground"] {
        let row = rows.iter().find(|r| r[0] == name).expect(name);
        assert_eq!(
            row.len(),
            9,
            "name, namespace, tenant, sim, hostname, shares-with, tenant-repo, tenant-ref, site \
             (the ninth since design b64c4377; a_tenant_site_per_instance.rs): {row:?}"
        );
    }
    let play = rows.iter().find(|r| r[0] == "playground").unwrap();
    assert_eq!(
        play[2], "examples/brewery",
        "the third column is the DIRECTORY the pod reads under /opt/boss"
    );
    assert_eq!(play[6], "", "no repo for an image-sourced instance");
    assert_eq!(play[7], "", "no ref for an image-sourced instance");
    let prod = rows.iter().find(|r| r[0] == "prod").unwrap();
    assert_eq!(prod[2], "tenant", "prod reads the delivered directory");
    assert_eq!(prod[6], "david/algedonic-llc");
    assert_eq!(prod[7], "main");

    let boss = std::fs::read_to_string(repo_root().join(BOSS_YAML)).unwrap();
    assert!(
        boss.contains("- {name: BOSS_TENANT_DIR, value: /opt/boss/tenant}"),
        "the boss container reads BOSS_TENANT_DIR — the delivered tenant (the source instance's value)"
    );
    assert!(
        !boss.contains("name: BOSS_TENANT_MANIFEST_TOML")
            && !boss.contains("name: BOSS_SIM_SEEDS_DIR"),
        "the manifest path and the seeds dir are DERIVED in the launcher, not set twice (§9a)"
    );
    assert!(
        boss.contains("mountPath: /opt/boss/tenant/seeds, readOnly: true"),
        "the delivered tenant mounts at /opt/boss/tenant/seeds — a sibling of nothing, \
         never a mount inside a read-only mount"
    );
    let vol = boss
        .find("- name: boss-tenant\n")
        .expect("the boss-tenant volume is declared");
    assert!(
        boss[vol..vol + 200].contains("name: boss-tenant\n")
            && boss[vol..vol + 200].contains("optional: true"),
        "the ConfigMap is optional: an image-sourced instance has none and must still boot"
    );
    // The flip is documented where it happens, naming both lines.
    assert!(
        toml.contains("tenant_repo = \"david/algedonic-llc\"") && toml.contains("BOSS_TENANT_DIR"),
        "the file's comment names the flip: prod's tenant_repo line and boss.yaml's BOSS_TENANT_DIR"
    );
}

#[test]
fn a_repo_sourced_instance_renders_the_delivered_mount_and_a_bad_source_is_refused() {
    let tree = fixture_tree("repo-source");
    playground_on_a_repo(&tree);
    let (rc, out, err) = run_renderer(&tree, &["--instances"]);
    assert_eq!(rc, 0, "{err}");
    let rows: Vec<Vec<&str>> = out.lines().map(|l| l.split('\t').collect()).collect();
    let play = rows.iter().find(|r| r[0] == "playground").unwrap();
    assert_eq!(
        play[2], "tenant",
        "a repo-sourced instance reads /opt/boss/tenant — the delivered mount"
    );
    assert_eq!(play[6], "david/tenant-fixture");
    assert_eq!(play[7], "main");
    let prod = rows.iter().find(|r| r[0] == "prod").unwrap();
    assert_eq!(prod[2], "tenant", "prod is repo-sourced since the flip");

    let out_dir = tree.join("out");
    let (rc, _, err) = run_renderer(&tree, &["--all", out_dir.to_str().unwrap()]);
    assert_eq!(rc, 0, "{err}");
    let rendered = std::fs::read_to_string(out_dir.join("boss-playground/boss.yaml")).unwrap();
    assert!(
        rendered.contains("- {name: BOSS_TENANT_DIR, value: /opt/boss/tenant}"),
        "the playground's pod reads the delivered directory: {rendered}"
    );
    assert!(
        !rendered.contains("/opt/boss/examples/brewery"),
        "no baked path survives into a repo-sourced instance"
    );
    let prod_rendered = std::fs::read_to_string(out_dir.join("boss/boss.yaml")).unwrap();
    assert_eq!(
        prod_rendered,
        std::fs::read_to_string(tree.join(BOSS_YAML)).unwrap(),
        "the source instance's render is still the identity"
    );

    // The refusals, each by name. `tenant =` is the retired key: a
    // stale line must not be read as "no source".
    let cases: &[(&str, &str, &[&str])] = &[
        (
            "retired-key",
            "tenant_dir = \"examples/brewery\"\nsim = true",
            &[
                "tenant_dir",
                "tenant = \"examples/brewery/seeds/tenant.toml\"\nsim = true",
            ],
        ),
        (
            "both-sources",
            "tenant_dir = \"examples/brewery\"\nsim = true",
            &[
                "tenant_dir",
                "tenant_dir = \"examples/brewery\"\ntenant_repo = \"david/x\"\ntenant_ref = \"main\"\nsim = true",
            ],
        ),
        (
            "repo-without-ref",
            "tenant_dir = \"examples/brewery\"\nsim = true",
            &["tenant_ref", "tenant_repo = \"david/x\"\nsim = true"],
        ),
        (
            "ref-without-repo",
            "tenant_dir = \"examples/brewery\"\nsim = true",
            &[
                "tenant_repo",
                "tenant_dir = \"examples/brewery\"\ntenant_ref = \"main\"\nsim = true",
            ],
        ),
        (
            "no-source",
            "tenant_dir = \"examples/brewery\"\nsim = true",
            &["tenant_dir", "sim = true"],
        ),
        (
            "bad-repo",
            "tenant_dir = \"examples/brewery\"\nsim = true",
            &[
                "tenant_repo",
                "tenant_repo = \"not-owner-slash-name\"\ntenant_ref = \"main\"\nsim = true",
            ],
        ),
        (
            "no-manifest",
            "tenant_dir = \"examples/brewery\"\nsim = true",
            &["tenant.toml", "tenant_dir = \"examples/empty\"\nsim = true"],
        ),
    ];
    for (case, from, to) in cases {
        let tree = fixture_tree(case);
        std::fs::create_dir_all(tree.join("examples/empty")).unwrap();
        let toml = instances_toml(&tree);
        let (head, play) = toml.split_once("[playground]").unwrap();
        let play = play.replacen(from, to[1], 1);
        assert_ne!(play, toml, "{case}: the fixture perturbs the playground");
        write_file(&tree.join(INSTANCES), &format!("{head}[playground]{play}"));
        let out_dir = tree.join("out");
        for args in [
            vec!["--instances"],
            vec!["--all", out_dir.to_str().unwrap()],
        ] {
            let (rc, out, err) = run_renderer(&tree, &args);
            assert_eq!(rc, REFUSED, "{case} {args:?}: refused, got rc {rc}: {err}");
            assert!(out.is_empty(), "{case}: a refusal renders nothing: {out}");
            assert!(
                err.contains(to[0]) && err.contains("[playground]"),
                "{case}: the refusal names the key and the instance: {err}"
            );
        }
        assert!(!out_dir.exists(), "{case}: nothing rendered");
    }
}

/// A fixture forge: bare repos under `forge/david/`, a checkout of the
/// product with a `forgejo` remote pointing at its bare repo — the
/// shape the converge's clone on the forge host has.
struct Forge {
    root: PathBuf,
    checkout: PathBuf,
}

impl Forge {
    fn new(name: &str) -> Self {
        let root = scratch_dir(&format!("tenant-source-forge-{name}"));
        let bare = root.join("forge/david");
        std::fs::create_dir_all(&bare).unwrap();
        let git = |dir: &Path, args: &[&str]| {
            let out = Command::new("git")
                .args(args)
                .current_dir(dir)
                .env("GIT_AUTHOR_NAME", "fixture")
                .env("GIT_AUTHOR_EMAIL", "fixture@example")
                .env("GIT_COMMITTER_NAME", "fixture")
                .env("GIT_COMMITTER_EMAIL", "fixture@example")
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "git {args:?} in {}: {}",
                dir.display(),
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        // The product: one commit, pushed to forge/david/boss.git.
        let product = root.join("product-src");
        std::fs::create_dir_all(&product).unwrap();
        write_file(&product.join("README.md"), "product\n");
        git(&product, &["init", "-q", "-b", "main"]);
        git(&product, &["add", "."]);
        git(&product, &["commit", "-qm", "product"]);
        git(&bare, &["init", "-q", "--bare", "boss.git"]);
        git(
            &product,
            &[
                "push",
                "-q",
                bare.join("boss.git").to_str().unwrap(),
                "main",
            ],
        );
        // The tenant: tenant.toml at the root and seeds/ beside it (the
        // real tenant's shape), pushed to forge/david/tenant-fixture.git.
        let tenant = root.join("tenant-src");
        std::fs::create_dir_all(tenant.join("seeds")).unwrap();
        write_file(
            &tenant.join("tenant.toml"),
            "[meta]\ntenant_id = \"fixture\"\ndisplay_name = \"Fixture\"\n",
        );
        write_file(
            &tenant.join("seeds/workflows.toml"),
            "[[workflow]]\nkind = \"x\"\n",
        );
        write_file(&tenant.join("seeds/classes.json"), "[]\n");
        write_file(&tenant.join("README.md"), "tenant prose, not a seed\n");
        git(&tenant, &["init", "-q", "-b", "main"]);
        git(&tenant, &["add", "."]);
        git(&tenant, &["commit", "-qm", "tenant"]);
        git(&bare, &["init", "-q", "--bare", "tenant-fixture.git"]);
        git(
            &tenant,
            &[
                "push",
                "-q",
                bare.join("tenant-fixture.git").to_str().unwrap(),
                "main",
            ],
        );
        // The converge's checkout of the product, with the forgejo remote
        // named as the runner expects it.
        let checkout = root.join("boss");
        git(
            &root,
            &[
                "clone",
                "-q",
                "-o",
                "forgejo",
                bare.join("boss.git").to_str().unwrap(),
                "boss",
            ],
        );
        Self { root, checkout }
    }

    fn tenant_sha(&self) -> String {
        let out = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(self.root.join("tenant-src"))
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// Source the lib and run one body.
    fn run(&self, body: &str, env: &[(&str, &str)]) -> (i32, String, String) {
        let script = format!(". '{}'\n{}\n", repo_root().join(LIB).display(), body);
        let mut cmd = Command::new("bash");
        cmd.arg("-c")
            .arg(&script)
            .env("REPO", &self.checkout)
            .env("GIT_TERMINAL_PROMPT", "0")
            .current_dir(&self.root);
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

#[test]
fn the_converge_derives_the_tenant_url_from_its_own_remote_and_measures_readability() {
    let f = Forge::new("readable");
    let bare = f.root.join("forge/david");
    // The URL: the checkout's forgejo remote with the repo path replaced
    // — same host, same scheme, same credential.
    let (rc, out, err) = f.run(r#"tenant_repo_url "$REPO" david/tenant-fixture"#, &[]);
    assert_eq!(rc, 0, "{err}");
    assert_eq!(
        out.trim(),
        bare.join("tenant-fixture.git").to_str().unwrap(),
        "the tenant URL is the forgejo remote's URL with the repo path replaced"
    );
    // The derivation on the URL shapes a forge remote takes, with the
    // credential kept exactly where it was — pure string work, no clone.
    for (remote, want) in [
        (
            "http://TOKEN@10.20.0.15:3000/david/boss.git",
            "http://TOKEN@10.20.0.15:3000/david/algedonic-llc.git",
        ),
        (
            "https://david:TOKEN@forge.example/david/boss",
            "https://david:TOKEN@forge.example/david/algedonic-llc.git",
        ),
        (
            "ssh://git@forge.example:2222/david/boss.git",
            "ssh://git@forge.example:2222/david/algedonic-llc.git",
        ),
        (
            "git@forge.example:david/boss.git",
            "git@forge.example:david/algedonic-llc.git",
        ),
    ] {
        let (rc, out, err) = f.run(
            &format!(r#"_tenant_url_from_remote '{remote}' david/algedonic-llc"#),
            &[],
        );
        assert_eq!(rc, 0, "{remote}: {err}");
        assert_eq!(out.trim(), want, "{remote}");
    }
    // No forgejo remote: cannot derive, rc 2, never a guess.
    let (rc, out, _) = f.run(
        r#"git -C "$REPO" remote rename forgejo origin; tenant_repo_url "$REPO" david/tenant-fixture"#,
        &[],
    );
    assert_eq!(rc, 2, "no forgejo remote is CANNOT DERIVE");
    assert!(out.trim().is_empty());

    // Readability is MEASURED: ls-remote answers the ref's sha.
    let f = Forge::new("measure");
    let (rc, out, err) = f.run(
        r#"tenant_source_check "$REPO" david/tenant-fixture main"#,
        &[],
    );
    assert_eq!(rc, 0, "{err}");
    assert_eq!(
        out.trim(),
        f.tenant_sha(),
        "the sha the ref resolves to, read not typed"
    );
    // A repo the credential cannot read (here: one that does not exist)
    // is unreadable — rc 1, the reason on stderr.
    let (rc, out, err) = f.run(r#"tenant_source_check "$REPO" david/private main"#, &[]);
    assert_eq!(rc, 1, "an unreadable repo is rc 1: {err}");
    assert!(out.trim().is_empty(), "no sha for an unreadable source");
    assert!(!err.trim().is_empty(), "the reason is said");
    // A ref the repo does not have is the same verdict.
    let (rc, _, err) = f.run(
        r#"tenant_source_check "$REPO" david/tenant-fixture no-such-branch"#,
        &[],
    );
    assert_eq!(rc, 1, "a missing ref is unreadable: {err}");
    // And the checkout: a shallow clone of the ref, fresh each time.
    let dir = f.root.join("tenants/playground");
    let (rc, _, err) = f.run(
        &format!(
            r#"tenant_checkout "$REPO" david/tenant-fixture main '{}'"#,
            dir.display()
        ),
        &[],
    );
    assert_eq!(rc, 0, "{err}");
    assert!(dir.join("tenant.toml").is_file() && dir.join("seeds/workflows.toml").is_file());
    write_file(&dir.join("stale-file"), "from a previous converge\n");
    let (rc, _, err) = f.run(
        &format!(
            r#"tenant_checkout "$REPO" david/tenant-fixture main '{}'"#,
            dir.display()
        ),
        &[],
    );
    assert_eq!(rc, 0, "{err}");
    assert!(
        !dir.join("stale-file").exists(),
        "the checkout is replaced, not layered: a stale file answering for a moved ref is the wrong-target class"
    );
    let (rc, _, err) = f.run(
        &format!(
            r#"tenant_checkout "$REPO" david/private main '{}'"#,
            dir.display()
        ),
        &[],
    );
    assert_eq!(rc, 1, "an unreadable checkout is rc 1: {err}");

    // A URL with a credential never reaches a journal: every message
    // goes through the redaction, and the redaction hides the userinfo.
    let (rc, out, _) = f.run(
        r#"printf '%s\n' "fatal: repository 'http://abc123:x-oauth@10.20.0.15:3000/david/private.git/' not found" | redact_url"#,
        &[],
    );
    assert_eq!(rc, 0);
    assert_eq!(
        out.trim(),
        "fatal: repository 'http://<redacted>@10.20.0.15:3000/david/private.git/' not found"
    );
    let lib = std::fs::read_to_string(repo_root().join(LIB)).unwrap();
    let start = lib.find("tenant_source_check() {").unwrap();
    let end = lib[start..].find("\n}\n").unwrap();
    assert!(
        lib[start..start + end].contains("redact_url"),
        "the check's stderr goes through the redaction"
    );
    let start = lib.find("tenant_checkout() {").unwrap();
    let end = lib[start..].find("\n}\n").unwrap();
    assert!(
        lib[start..start + end].contains("redact_url"),
        "the checkout's stderr goes through the redaction"
    );
}

#[test]
fn the_tenant_directory_is_staged_flat_for_one_configmap() {
    let root = scratch_dir("tenant-source-stage");
    let src = root.join("tenant");
    std::fs::create_dir_all(src.join("seeds/nested")).unwrap();
    write_file(&src.join("tenant.toml"), "[meta]\ntenant_id = \"root\"\n");
    write_file(
        &src.join("seeds/tenant.toml"),
        "[meta]\ntenant_id = \"seeds-copy\"\n",
    );
    write_file(&src.join("seeds/workflows.toml"), "[[workflow]]\n");
    write_file(&src.join("seeds/nested/ignored.json"), "[]\n");
    write_file(&src.join("README.md"), "prose\n");
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
            String::from_utf8_lossy(&out.stderr).to_string(),
        )
    };
    let (rc, err) = run(r#"tenant_stage "$SRC" "$STAGE""#);
    assert_eq!(rc, 0, "{err}");
    let mut names: Vec<String> = std::fs::read_dir(&stage)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
        .collect();
    names.sort();
    assert_eq!(
        names,
        ["tenant.toml", "workflows.toml"],
        "every file of seeds/ plus the root manifest; no prose, no subdirectory (a ConfigMap is flat)"
    );
    assert_eq!(
        std::fs::read_to_string(stage.join("tenant.toml")).unwrap(),
        "[meta]\ntenant_id = \"root\"\n",
        "the root tenant.toml is the canonical one and wins over a seeds/ copy"
    );
    // The seeds/tenant.toml spelling alone is accepted too.
    std::fs::remove_file(src.join("tenant.toml")).unwrap();
    std::fs::remove_dir_all(&stage).unwrap();
    let (rc, err) = run(r#"tenant_stage "$SRC" "$STAGE""#);
    assert_eq!(rc, 0, "{err}");
    assert_eq!(
        std::fs::read_to_string(stage.join("tenant.toml")).unwrap(),
        "[meta]\ntenant_id = \"seeds-copy\"\n"
    );
    // No manifest at all: refused, nothing staged.
    std::fs::remove_file(src.join("seeds/tenant.toml")).unwrap();
    std::fs::remove_dir_all(&stage).unwrap();
    let (rc, err) = run(r#"tenant_stage "$SRC" "$STAGE""#);
    assert_eq!(rc, 2, "a directory with no manifest is refused: {err}");
    assert!(err.contains("tenant.toml"), "{err}");
    assert!(!stage.exists(), "nothing staged");
    // Over a ConfigMap's size: refused with the number.
    write_file(&src.join("tenant.toml"), "[meta]\n");
    write_file(&src.join("seeds/big.json"), &"x".repeat(1_100_000));
    let (rc, err) = run(r#"tenant_stage "$SRC" "$STAGE""#);
    assert_eq!(rc, 2, "a tenant a ConfigMap cannot hold is refused: {err}");
    assert!(err.contains("MiB") || err.contains("bytes"), "{err}");
    assert!(!stage.exists(), "nothing staged");
}

#[test]
fn the_runner_measures_before_it_provisions_and_delivers_before_it_applies() {
    let src = std::fs::read_to_string(repo_root().join(RUNNER)).unwrap();
    // The source instance: measured and delivered before its apply,
    // and an unreadable source FAILS the converge (nothing rolled).
    let prod_apply = src
        .find("apply_instance \"$SOURCE_NS\"")
        .expect("the source instance is applied");
    let prod_tenant = src
        .find("converge_tenant \"$SOURCE_NAME\"")
        .expect("the source instance's tenant is converged by name");
    assert!(
        prod_tenant < prod_apply,
        "the source's tenant is delivered before its manifests are applied"
    );
    // The other instances: measured first, before the quiet secret read
    // — an instance whose tenant cannot be read is skipped whole before
    // anything is minted for it — and delivered after the loud gate,
    // before the apply.
    let loop_start = src
        .find("STAGE=\"apply instances\"")
        .expect("the runner has the apply-instances stage");
    let after = &src[loop_start..];
    let measure = after
        .find("tenant_source_verdict")
        .expect("the loop measures the tenant source");
    let quiet = after.find("$(instance_secrets_absent ").unwrap();
    let gate = after.find("$(instance_secret_gate ").unwrap();
    let deliver = after
        .find("converge_tenant \"$iname\"")
        .expect("the loop delivers the instance's tenant");
    let apply = after.find("apply_instance \"$ins_ns\"").unwrap();
    assert!(
        measure < quiet && gate < deliver && deliver < apply,
        "measure → (secrets) → gate → deliver → apply"
    );
    assert!(
        after[measure..quiet].contains("skipped_tenant_entry"),
        "an unreadable tenant is a skipped entry in the ONE spelling"
    );
    assert!(
        after.contains("run_summary_field tenant_source"),
        "the verdict rides the packet as `tenant_source`"
    );
    assert!(
        after.contains("IFS=\"$IFS_ROW\" read -r iname ins_ns tdir _s _h share_ns trepo tref"),
        "the loop reads the tenant columns off the instance list, empty columns kept"
    );
    // The lib's one spelling, parsed by the one parser.
    let out = Command::new("bash")
        .arg("-c")
        .arg(format!(
            ". '{}'\n. '{}'\nBOSS_INSTANCES_SKIPPED=\"$(skipped_tenant_entry boss-x unreadable david/x main); boss-y (secrets absent: a)\"\nprintf '%s\\n' \"$BOSS_INSTANCES_SKIPPED\"\nskip_reason boss-x\nskip_reason boss-y\n",
            repo_root().join(LIB).display(),
            repo_root().join(SKIPPED_LIB).display()
        ))
        .output()
        .unwrap();
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    let mut lines = text.lines();
    assert_eq!(
        lines.next().unwrap(),
        "boss-x (tenant source unreadable: david/x@main); boss-y (secrets absent: a)"
    );
    assert_eq!(lines.next().unwrap(), "tenant source unreadable");
    assert_eq!(lines.next().unwrap(), "secrets absent");
}

/// Measured 2026-09-16 23:34Z on the first converge after the prod flip:
/// `IFS=$'\t' read` collapses a run of tabs, so prod's EMPTY
/// `shares_with` column shifted `tenant_repo` into `tenant_ref`, the
/// runner asked the forge for repository `main`, and the converge ended
/// `tenant_source: boss: unreadable (main@)` with nothing rolled. Every
/// reader of the instance list now goes through `instance_rows`, which
/// keeps an empty column as a column.
#[test]
fn an_empty_instance_column_keeps_its_place_when_the_runner_reads_the_row() {
    let (rc, out, err) = run_renderer(&repo_root(), &["--instances"]);
    assert_eq!(rc, 0, "{err}");
    let prod = out
        .lines()
        .find(|l| l.starts_with("prod\t"))
        .expect("prod row");
    assert!(
        prod.contains("\t\t"),
        "the measured shape: prod carries an empty column (shares_with) before its repo: {prod:?}"
    );
    let script = format!(
        // Every column named, the site (ninth) included: `read` folds the
        // rest of a row into its LAST variable, so a reader one column
        // short would take `main<US>www…` for the ref (measured when the
        // site column landed, b64c4377).
        ". '{}'\nwhile IFS=\"$IFS_ROW\" read -r iname ins_ns tdir _s _h _share trepo tref site; do printf '%s|%s|%s|%s\\n' \"$iname\" \"$tdir\" \"$trepo\" \"$tref\"; done <<< \"$(instance_rows \"$INSTANCES\")\"\n",
        repo_root().join(LIB).display()
    );
    let read = Command::new("bash")
        .arg("-c")
        .arg(&script)
        .env("INSTANCES", &out)
        .env("REPO", repo_root())
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&read.stdout);
    assert!(
        read.status.success(),
        "{}",
        String::from_utf8_lossy(&read.stderr)
    );
    assert!(
        text.lines()
            .any(|l| l == "prod|tenant|david/algedonic-llc|main"),
        "prod's repo and ref survive the empty column: {text}"
    );
    assert!(
        text.lines().any(|l| l == "playground|examples/brewery||"),
        "an image-sourced row keeps its empty repo and ref: {text}"
    );
    // The runner and the lib read every row this way — no tab-IFS read
    // of the instance list remains.
    let runner = std::fs::read_to_string(repo_root().join(RUNNER)).unwrap();
    let lib = std::fs::read_to_string(repo_root().join(LIB)).unwrap();
    for (name, src) in [("runner", runner.as_str()), ("lib", lib.as_str())] {
        assert!(
            !src.contains("IFS=$'\\t' read -r iname"),
            "{name} still reads an instance row with a tab IFS, which drops an empty column"
        );
    }
    assert_eq!(
        runner
            .matches("done <<< \"$(instance_rows \"$INSTANCES\")\"")
            .count(),
        3,
        "the runner's three instance loops read through instance_rows"
    );
}

#[test]
fn the_no_op_tick_counts_an_undelivered_tenant_as_skipped() {
    // instances_skipped_by_gate reads the cluster only: a repo-sourced
    // instance whose boss-tenant ConfigMap is absent was never applied,
    // and its hostname must stay served by the source (40d46042).
    let root = scratch_dir("tenant-source-noop");
    let kubectl = root.join("kubectl");
    write_exec(
        &kubectl,
        r#"#!/usr/bin/env bash
echo "kubectl $*" >> "$STUB_LOG"
all="$*"
case "$all" in
  *"create --dry-run=client -o json -f "*) echo '{"kind":"Deployment","metadata":{"name":"boss"},"spec":{"template":{"spec":{"containers":[]}}}}' ;;
  *"get configmap boss-tenant -n "*)
    ns=$(printf '%s\n' "$@" | grep -A1 -x -- -n | tail -n1)
    if grep -qx -- "$ns" "$STUB_DELIVERED"; then echo "NAME DATA"; exit 0; fi
    echo "Error from server (NotFound): configmaps \"boss-tenant\" not found" >&2; exit 1 ;;
esac
exit 0
"#,
    );
    let delivered = root.join("delivered");
    write_file(&delivered, "");
    let instances = "prod\tboss\texamples/brewery\tfalse\tboss.example\t\t\t\n\
                     play\tboss-play\ttenant\ttrue\tplay.example\tboss\tdavid/x\tmain\n\
                     img\tboss-img\texamples/brewery\ttrue\timg.example\tboss\t\t\n";
    let run = || {
        let out = Command::new("bash")
            .arg("-c")
            .arg(format!(
                ". '{}'\ninstances_skipped_by_gate \"$K\" \"$K\" boss \"$INSTANCES\" /manifests\n",
                repo_root().join(LIB).display()
            ))
            .env("K", &kubectl)
            .env("INSTANCES", instances)
            .env("STUB_LOG", root.join("calls"))
            .env("STUB_DELIVERED", &delivered)
            .output()
            .unwrap();
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).trim().to_string(),
            String::from_utf8_lossy(&out.stderr).to_string(),
        )
    };
    let (rc, out, err) = run();
    assert_eq!(rc, 0, "{err}");
    assert_eq!(
        out, "boss-play (tenant not delivered: david/x@main)",
        "the repo-sourced instance with no ConfigMap is skipped; the image-sourced one is not asked"
    );
    let calls = std::fs::read_to_string(root.join("calls")).unwrap();
    assert!(
        !calls.contains("-n boss-img") || !calls.contains("configmap"),
        "an image-sourced instance is never asked for a ConfigMap: {calls}"
    );
    write_file(&delivered, "boss-play\n");
    let (rc, out, err) = run();
    assert_eq!(rc, 0, "{err}");
    assert_eq!(out, "", "delivered: nothing skipped");
}

#[test]
fn the_launcher_derives_the_manifest_and_the_seeds_from_the_tenant_dir() {
    let root = scratch_dir("tenant-source-launcher");
    // A PATH with no service binary and a stub config generator, so the
    // launcher starts nothing and writes nothing under /etc.
    let bin = root.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    write_exec(
        &bin.join("boss-generate-configs"),
        "#!/usr/bin/env bash\necho stub-configs\n",
    );
    let path = format!("{}:/usr/bin:/bin", bin.display());
    let run = |env: &[(&str, &str)]| {
        let mut cmd = Command::new("bash");
        cmd.arg(repo_root().join(LAUNCHER))
            .env_clear()
            .env("PATH", &path)
            .env("HOME", &root);
        for (k, v) in env {
            cmd.env(k, v);
        }
        let out = cmd.output().unwrap();
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).to_string(),
            String::from_utf8_lossy(&out.stderr).to_string(),
        )
    };
    // The real tenant's shape: tenant.toml at the root.
    let at_root = root.join("at-root");
    std::fs::create_dir_all(at_root.join("seeds")).unwrap();
    write_file(&at_root.join("tenant.toml"), "[meta]\n");
    // With no service binary on PATH the launcher's `wait -n` has no
    // child and exits 127 under set -e — the baseline, measured; what
    // this pins is that it got PAST the derivation to the start loop.
    let (_, out, err) = run(&[("BOSS_TENANT_DIR", at_root.to_str().unwrap())]);
    assert!(out.contains("==> all services up"), "{err}\n{out}");
    assert!(
        out.contains(&format!(
            "BOSS_TENANT_MANIFEST_TOML={}",
            at_root.join("tenant.toml").display()
        )),
        "the manifest is the root tenant.toml: {out}"
    );
    assert!(
        out.contains(&format!(
            "BOSS_SIM_SEEDS_DIR={}",
            at_root.join("seeds").display()
        )),
        "the seeds dir is derived beside it: {out}"
    );
    // The examples' shape: seeds/tenant.toml.
    let in_seeds = root.join("in-seeds");
    std::fs::create_dir_all(in_seeds.join("seeds")).unwrap();
    write_file(&in_seeds.join("seeds/tenant.toml"), "[meta]\n");
    let (_, out, err) = run(&[("BOSS_TENANT_DIR", in_seeds.to_str().unwrap())]);
    assert!(out.contains("==> all services up"), "{err}\n{out}");
    assert!(
        out.contains(&format!(
            "BOSS_TENANT_MANIFEST_TOML={}",
            in_seeds.join("seeds/tenant.toml").display()
        )),
        "{out}"
    );
    // An explicit value wins (the compose file and bootstrap-local.sh
    // set the paths directly).
    let (_, out, err) = run(&[
        ("BOSS_TENANT_DIR", in_seeds.to_str().unwrap()),
        ("BOSS_TENANT_MANIFEST_TOML", "/elsewhere/tenant.toml"),
        ("BOSS_SIM_SEEDS_DIR", "/elsewhere/seeds"),
    ]);
    assert!(out.contains("==> all services up"), "{err}\n{out}");
    assert!(
        out.contains("BOSS_TENANT_MANIFEST_TOML=/elsewhere/tenant.toml")
            && out.contains("BOSS_SIM_SEEDS_DIR=/elsewhere/seeds"),
        "{out}"
    );
    // A directory with no manifest refuses the launch, by path, before
    // anything starts: a pod on the gateway's default path would answer
    // instead of erroring.
    let empty = root.join("empty");
    std::fs::create_dir_all(&empty).unwrap();
    let (rc, out, err) = run(&[("BOSS_TENANT_DIR", empty.to_str().unwrap())]);
    assert_eq!(rc, 1, "refused: {err}");
    assert!(
        err.contains(empty.to_str().unwrap()) && err.contains("tenant.toml"),
        "the refusal names the directory and what it lacks: {err}"
    );
    assert!(
        !out.contains("stub-configs") && !out.contains("==> all services up"),
        "nothing started: {out}"
    );
    // Unset: today's behaviour, nothing derived, nothing refused.
    let (_, out, err) = run(&[]);
    assert!(out.contains("==> all services up"), "{err}\n{out}");
    assert!(!out.contains("BOSS_TENANT_MANIFEST_TOML"), "{out}");
}
