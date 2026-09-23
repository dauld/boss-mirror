//! The demo-agents block is the BREWERY's, not every instance's
//! (backlog b03f38de, car 1 of e2580840; measured 2026-09-17).
//!
//! infra/oss-quickstart/generate-configs.sh wrote `[demo_agents]` into
//! boss-observability's config unconditionally, so prod — whose tenant
//! is Algedonic, LLC, not the brewery — ran the synthetic telemetry
//! loop, ticking "Hop sourcing scout" spend figures every 8 s into its
//! log and answering `demo_mode: true` on /api/snapshot. The block is
//! the brewery playground's: it exists so `/ops` shows what agent
//! oversight LOOKS like before a real boss-cybernetics is wired in.
//!
//! Now the generator finds the tenant directory from the manifest the
//! launcher already derived for it (BOSS_TENANT_MANIFEST_TOML, from
//! BOSS_TENANT_DIR) and writes the block only when that tenant ships a
//! demo roster, `seeds/demo_agents.toml`, which the block names. Until
//! 2026-09-23 the switch was `[meta] tenant_id == "brewery"` and the
//! roster itself was a literal in the Tier 1 boss-observability crate;
//! backlog 1c68aebc moved the agents into the brewery's bundle and made
//! the file the switch. No manifest, an unreadable one, or a tenant
//! with no roster means no block — and boss-observability treats an
//! absent block as off. The generator is RUN here against the tree's
//! own two tenant manifests and a stub `boss-ports-list`, so each
//! verdict is one it reached.

use boss_testing::{repo_root, scratch_dir, write_exec};
use std::path::Path;
use std::process::Command;

const GENERATOR: &str = "infra/oss-quickstart/generate-configs.sh";
const BREWERY: &str = "examples/brewery/seeds/tenant.toml";
const NOT_BREWERY: &str = "examples/used-device-shop/seeds/tenant.toml";
const BREWERY_ROSTER: &str = "examples/brewery/seeds/demo_agents.toml";

/// Every name the generator's `p` and `PORT[...]` lookups ask for,
/// with made-up ports: the script refuses an unknown name (`:?`), so a
/// missing entry here fails loudly rather than skipping a file.
const STUB_PORTS: &str = "#!/usr/bin/env bash
case \"${1:-}\" in
  --paired)
    for n in shipping messages inventory commerce people accounts assets catalog calendar jobs; do
      echo \"$n:7000:8000\"
    done ;;
  --solo)
    for n in ml ledger content policy classes locations subject-kinds events products campaigns customers observability; do
      echo \"$n:7100\"
    done ;;
  *) echo \"stub boss-ports-list: unknown flag ${1:-}\" >&2; exit 2 ;;
esac
";

/// Run the generator into a scratch ETC_DIR with exactly the given
/// environment (plus PATH and ETC_DIR) and return that ETC_DIR.
fn generate(case: &str, env: &[(&str, &std::ffi::OsStr)]) -> std::path::PathBuf {
    let root = scratch_dir(&format!("generate-configs-{case}"));
    let bin = root.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    write_exec(&bin.join("boss-ports-list"), STUB_PORTS);
    let etc = root.join("etc");
    std::fs::create_dir_all(&etc).unwrap();
    let mut cmd = Command::new("bash");
    cmd.arg(repo_root().join(GENERATOR))
        .env_clear()
        .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
        .env("ETC_DIR", &etc);
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("bash runs the generator");
    assert!(
        out.status.success(),
        "generate-configs.sh ({case}) refused: {}\n{}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout)
    );
    etc
}

/// Run the generator with the given manifest path (None: the variable
/// unset) and return boss-observability.toml.
fn observability_config(case: &str, manifest: Option<&Path>) -> String {
    let env: Vec<(&str, &std::ffi::OsStr)> = manifest
        .map(|m| ("BOSS_TENANT_MANIFEST_TOML", m.as_os_str()))
        .into_iter()
        .collect();
    let etc = generate(case, &env);
    let path = etc.join("boss-observability.toml");
    let text =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    // Whatever the tenant, the file must still be the config the
    // service parses — a block dropped by breaking the heredoc would
    // pass a "no demo_agents" assertion for the wrong reason.
    let parsed: toml::Value = toml::from_str(&text)
        .unwrap_or_else(|e| panic!("{} is not TOML: {e}\n{text}", path.display()));
    assert_eq!(
        parsed.get("bind").and_then(|v| v.as_str()),
        Some("0.0.0.0:7100"),
        "{case}: bind comes from the port table: {text}"
    );
    text
}

/// The `[demo_agents] roster` the generated config names, if any.
fn roster_of(text: &str) -> Option<String> {
    let parsed: toml::Value = toml::from_str(text).ok()?;
    parsed
        .get("demo_agents")?
        .get("roster")?
        .as_str()
        .map(str::to_owned)
}

#[test]
fn the_brewery_gets_the_demo_agents_block() {
    let text = observability_config("brewery", Some(&repo_root().join(BREWERY)));
    assert!(
        text.contains("[demo_agents]"),
        "the brewery's config carries the synthetic-agent block: {text}"
    );
    // The block names the tenant's OWN roster (backlog 1c68aebc): the
    // agents live in examples/brewery, not in the Tier 1 crate that
    // renders them, and the path is absolute because boss-observability
    // resolves it from wherever it was started.
    let roster = repo_root().join(BREWERY_ROSTER);
    assert_eq!(
        roster_of(&text).as_deref(),
        Some(roster.to_str().unwrap()),
        "{text}"
    );
    assert!(roster.is_file(), "{} must exist", roster.display());
}

/// The switch is the FILE, not the tenant's name (backlog 1c68aebc):
/// any tenant that ships `seeds/demo_agents.toml` beside its manifest
/// gets the block naming it, at either manifest spelling the contract
/// accepts — so a second playground needs a file, never an edit to
/// this generator.
#[test]
fn a_tenant_that_ships_a_roster_gets_the_block_whatever_its_name() {
    for (case, manifest_rel) in [
        ("lab-seeds", "seeds/tenant.toml"),
        ("lab-root", "tenant.toml"),
    ] {
        let dir = scratch_dir(&format!("generate-configs-{case}"));
        std::fs::create_dir_all(dir.join("seeds")).unwrap();
        std::fs::write(
            dir.join(manifest_rel),
            "[meta]\ntenant_id = \"lab\"\nname = \"A lab\"\n",
        )
        .unwrap();
        std::fs::write(dir.join("seeds/demo_agents.toml"), "").unwrap();
        let text = observability_config(case, Some(&dir.join(manifest_rel)));
        let want = dir.join("seeds/demo_agents.toml").canonicalize().unwrap();
        assert_eq!(
            roster_of(&text).as_deref(),
            Some(want.to_str().unwrap()),
            "{case}: {text}"
        );
    }
}

#[test]
fn another_tenant_gets_no_demo_agents_block() {
    let text = observability_config("not-brewery", Some(&repo_root().join(NOT_BREWERY)));
    assert!(
        !text.contains("demo_agents"),
        "a non-brewery tenant's config must not run the demo loop: {text}"
    );
}

#[test]
fn no_manifest_means_no_demo_agents_block() {
    // The default is OFF: a deployment that names no tenant (bare
    // generate-configs, the N-1 launcher with nothing set) gets the
    // real aggregator path, never synthetic figures.
    let text = observability_config("no-manifest", None);
    assert!(!text.contains("demo_agents"), "{text}");
    let missing = scratch_dir("generate-configs-missing-manifest").join("absent/tenant.toml");
    let text = observability_config("missing-manifest", Some(&missing));
    assert!(
        !text.contains("demo_agents"),
        "an unreadable manifest is not the brewery: {text}"
    );
}

/// The tree's bundles are the fixtures above, so the verdicts depend on
/// which of them ships a roster; pin both halves.
#[test]
fn the_fixture_bundles_ship_the_rosters_this_test_assumes() {
    assert!(repo_root().join(BREWERY_ROSTER).is_file());
    let other = repo_root()
        .join(NOT_BREWERY)
        .with_file_name("demo_agents.toml");
    assert!(!other.exists(), "{} must not exist", other.display());
}

// ---------------------------------------------------------------------
// file_refs (backlog 6280be03, measured 2026-09-23 on the tree at
// 509f165f). The content-addressed blob store was built and never
// switched on: this generator wrote boss-content-api's config with
// postgres_url, http_bind and nats_url only, so the service mounted its
// 503 "unconfigured" fallback on every /api/files path and the nightly
// files-gc no-opped. The store is on now — but ONLY where the
// deployment names a root, because the root must be DURABLE: a default
// path would put every attachment on the container's writable layer,
// and the next container would restore pointers to nothing.

/// Parse boss-content-api.toml from a generator run.
fn content_config(case: &str, files_root: Option<&str>) -> toml::Value {
    let env: Vec<(&str, &std::ffi::OsStr)> = files_root
        .map(|r| ("BOSS_FILES_ROOT", std::ffi::OsStr::new(r)))
        .into_iter()
        .collect();
    let etc = generate(case, &env);
    let path = etc.join("boss-content-api.toml");
    let text =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    toml::from_str(&text).unwrap_or_else(|e| panic!("{} is not TOML: {e}\n{text}", path.display()))
}

#[test]
fn a_named_files_root_switches_the_file_store_on() {
    let cfg = content_config("files-on", Some("/var/lib/boss/files"));
    assert_eq!(
        cfg.get("files")
            .and_then(|f| f.get("root"))
            .and_then(|v| v.as_str()),
        Some("/var/lib/boss/files"),
        "BOSS_FILES_ROOT must become the [files] root boss-content-api mounts \
         /api/files on: {cfg:?}"
    );
    // With [files] set and no policy_api_url, the service falls back to
    // PermissivePolicyClient — every caller may attach to anything. The
    // policy engine runs in the same container on the port boss-ports
    // assigns it (the stub table says 7100 for every solo service).
    assert_eq!(
        cfg.get("policy_api_url").and_then(|v| v.as_str()),
        Some("http://127.0.0.1:7100"),
        "a switched-on file store must be policy-checked, not permissive: {cfg:?}"
    );
}

#[test]
fn no_files_root_leaves_the_file_store_off() {
    let cfg = content_config("files-off", None);
    assert!(
        cfg.get("files").is_none(),
        "a deployment that names no durable root must get NO [files] block — a default path \
         would store attachments on the container's writable layer: {cfg:?}"
    );
    // The rest of the service is unchanged either way.
    assert!(cfg.get("postgres_url").is_some() && cfg.get("http_bind").is_some());
}
