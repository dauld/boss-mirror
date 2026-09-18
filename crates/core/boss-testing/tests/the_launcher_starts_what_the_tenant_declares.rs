//! The image and the launcher stop assuming the brewery (backlog
//! 18d6a6c9, design e2580840 car 5, 2026-09-17) — part (a).
//!
//! Measured on prod on 2026-09-17: with the brewery's 31 reactors moved
//! to examples/brewery/seeds/rules.toml, BOSS_EVENT_WEBHOOK_URL in
//! infra/cluster/manifests/boss.yaml pointed every instance's
//! dispatcher at a simulator callback port nothing listened on; the
//! Dockerfile refused to build an image without boss-brewery-sim; and
//! the launcher started the simulator, catalog, assets, inventory and
//! shipping services for a tenant whose manifest declares none of
//! those modules (0 events from each on prod).
//!
//! Now the launcher reads the tenant manifest's `[modules]` the way the
//! SPA does since ce68f137 (a module is on only when listed true) and
//! starts a module's service only when the tenant asks; the sim's
//! loopback pair (BOSS_SIM_CALLBACK_BIND + BOSS_EVENT_WEBHOOK_URL) is
//! derived when the sim runs and absent when it does not, so the
//! manifest carries neither; and the image build warns instead of
//! failing when the brewery's engine is not among the binaries.
//! Exercised through the launcher's `--plan` door, which prints the
//! decision per service and exits without starting anything.

use boss_testing::{create_dir, repo_root, scratch_dir, write_file};
use std::path::{Path, PathBuf};
use std::process::Command;

const LAUNCHER: &str = "infra/oss-quickstart/services-launcher.sh";
const MANIFEST: &str = "infra/cluster/manifests/boss.yaml";
const DOCKERFILE: &str = "infra/oss-quickstart/Dockerfile";

/// A tenant directory whose seeds/tenant.toml lists `modules` true —
/// with the comment-and-whitespace shapes the example manifests use,
/// so the shell reader is exercised on the real grammar.
fn tenant(root: &Path, name: &str, modules: &[&str]) -> PathBuf {
    let dir = root.join(name);
    create_dir(&dir.join("seeds"));
    let mut body = format!(
        "[meta]\ntenant_id = \"{name}\"\ndisplay_name = \"{name}\"\n\n[labels]\n\"jobs.title\" = \"Work\"\n\n[modules]\n# A module is ON only when listed true here.\njobs        = true   # Jobs + Steps\n"
    );
    for m in modules {
        body.push_str(&format!("{m:<12}= true   # declared\n"));
    }
    body.push_str("support     = false  # declared off\n\n[other]\nx = 1\n");
    write_file(&dir.join("seeds/tenant.toml"), &body);
    dir
}

fn plan(env: &[(&str, &str)]) -> (i32, String) {
    let mut cmd = Command::new("bash");
    cmd.arg(repo_root().join(LAUNCHER)).arg("--plan");
    for k in [
        "BOSS_TENANT_DIR",
        "BOSS_TENANT_MANIFEST_TOML",
        "BOSS_SIM_ENABLED",
        "BOSS_SIM_CALLBACK_BIND",
        "BOSS_EVENT_WEBHOOK_URL",
    ] {
        cmd.env_remove(k);
    }
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("run the launcher");
    (
        out.status.code().unwrap_or(-1),
        format!(
            "{}--- stderr\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

fn line(out: &str, prefix: &str) -> Option<String> {
    out.lines()
        .find(|l| l.starts_with(prefix))
        .map(str::to_string)
}

#[test]
fn a_tenant_that_declares_no_module_gets_no_module_service() {
    let root = scratch_dir("launcher-starts-what-the-tenant-declares-none");
    let acme = tenant(&root, "acme", &[]);
    let (rc, out) = plan(&[("BOSS_TENANT_DIR", &acme.display().to_string())]);
    assert_eq!(rc, 0, "{out}");
    for (svc, module) in [
        ("boss-simulator", "sim"),
        ("boss-catalog-api", "equipment"),
        ("boss-assets-api", "equipment"),
        ("boss-inventory-api", "warehouse"),
        ("boss-shipping-api", "shipping"),
    ] {
        assert_eq!(
            line(&out, &format!("skip {svc} ")).as_deref(),
            Some(format!("skip {svc} (module {module} is not on in the tenant manifest)").as_str()),
            "{svc} is the {module} module's service, which acme does not declare:\n{out}"
        );
    }
    // The platform's services start regardless — the gateway, the jobs
    // API, the dispatcher, the relay, the finance/commerce pair every
    // tenant's `finance = true` asks for.
    for svc in [
        "boss-gateway",
        "boss-jobs-api",
        "boss-dispatcher",
        "boss-event-relay",
        "boss-clock-api",
        "boss-policy-api",
        "boss-classes-api",
    ] {
        assert_eq!(
            line(&out, &format!("start {svc}")).as_deref(),
            Some(format!("start {svc}").as_str()),
            "{svc} is the platform's:\n{out}"
        );
    }
    // No sim: BOSS_SIM_ENABLED derives to false, and the sim's loopback
    // pair is NOT exported — the dispatcher's webhook.notify stays the
    // no-op it is wherever no external party is wired.
    assert_eq!(
        line(&out, "sim ").as_deref(),
        Some("sim BOSS_SIM_ENABLED=false (derived: the tenant manifest does not list sim = true)"),
        "{out}"
    );
    assert_eq!(
        line(&out, "webhook ").as_deref(),
        Some("webhook BOSS_EVENT_WEBHOOK_URL=unset BOSS_SIM_CALLBACK_BIND=unset"),
        "{out}"
    );
}

#[test]
fn a_tenant_that_declares_a_module_gets_its_service_and_the_sim_gets_its_loopback_pair() {
    let root = scratch_dir("launcher-starts-what-the-tenant-declares-some");
    let brew = tenant(&root, "brewery", &["sim", "equipment", "shipping"]);
    let (rc, out) = plan(&[("BOSS_TENANT_DIR", &brew.display().to_string())]);
    assert_eq!(rc, 0, "{out}");
    for svc in [
        "boss-simulator",
        "boss-catalog-api",
        "boss-assets-api",
        "boss-shipping-api",
    ] {
        assert_eq!(
            line(&out, &format!("start {svc}")).as_deref(),
            Some(format!("start {svc}").as_str()),
            "{out}"
        );
    }
    assert_eq!(
        line(&out, "skip boss-inventory-api ").as_deref(),
        Some("skip boss-inventory-api (module warehouse is not on in the tenant manifest)"),
        "warehouse is not declared:\n{out}"
    );
    // `support = false` is off the same as missing.
    assert!(!out.contains("start boss-support"), "{out}");
    assert_eq!(
        line(&out, "sim ").as_deref(),
        Some("sim BOSS_SIM_ENABLED=true (derived: the tenant manifest lists sim = true)"),
        "{out}"
    );
    assert_eq!(
        line(&out, "webhook ").as_deref(),
        Some(
            "webhook BOSS_EVENT_WEBHOOK_URL=http://127.0.0.1:7099/callback BOSS_SIM_CALLBACK_BIND=127.0.0.1:7099"
        ),
        "the sim's loopback pair is derived, one from the other:\n{out}"
    );
}

#[test]
fn an_explicit_deployment_value_wins_over_the_manifest() {
    // The cluster parks the sim with BOSS_SIM_ENABLED=false on an
    // instance whose tenant lists sim = true (2026-09-05, 59f063de);
    // the compose file sets the loopback pair outright. Neither is
    // overridden by the derivation.
    let root = scratch_dir("launcher-starts-what-the-tenant-declares-explicit");
    let brew = tenant(&root, "brewery", &["sim"]);
    let dir = brew.display().to_string();
    let (rc, out) = plan(&[("BOSS_TENANT_DIR", &dir), ("BOSS_SIM_ENABLED", "false")]);
    assert_eq!(rc, 0, "{out}");
    assert_eq!(
        line(&out, "sim ").as_deref(),
        Some("sim BOSS_SIM_ENABLED=false (set by the deployment)"),
        "{out}"
    );
    assert_eq!(
        line(&out, "webhook ").as_deref(),
        Some("webhook BOSS_EVENT_WEBHOOK_URL=unset BOSS_SIM_CALLBACK_BIND=unset"),
        "a parked sim listens on nothing, so nothing is forwarded to it:\n{out}"
    );
    // The /simulator UX is the `sim` module's, on whether the tenant
    // declares it — the parked tick daemon is a separate switch.
    assert_eq!(
        line(&out, "start boss-simulator").as_deref(),
        Some("start boss-simulator"),
        "{out}"
    );

    let (rc, out) = plan(&[
        ("BOSS_TENANT_DIR", &dir),
        ("BOSS_SIM_CALLBACK_BIND", "0.0.0.0:7100"),
        ("BOSS_EVENT_WEBHOOK_URL", "http://sim.internal:7100/cb"),
    ]);
    assert_eq!(rc, 0, "{out}");
    assert_eq!(
        line(&out, "webhook ").as_deref(),
        Some(
            "webhook BOSS_EVENT_WEBHOOK_URL=http://sim.internal:7100/cb BOSS_SIM_CALLBACK_BIND=0.0.0.0:7100"
        ),
        "{out}"
    );
}

#[test]
fn an_undeclared_tenant_keeps_the_old_roster() {
    // No BOSS_TENANT_DIR and no manifest path: the N-1 shape. Nothing
    // is derived and every service starts, as before — the launcher
    // never guesses a tenant's modules from an absent file.
    let (rc, out) = plan(&[]);
    assert_eq!(rc, 0, "{out}");
    assert!(!out.contains("\nskip "), "{out}");
    for svc in [
        "boss-simulator",
        "boss-catalog-api",
        "boss-shipping-api",
        "boss-gateway",
    ] {
        assert_eq!(
            line(&out, &format!("start {svc}")).as_deref(),
            Some(format!("start {svc}").as_str()),
            "{out}"
        );
    }
    assert_eq!(
        line(&out, "sim ").as_deref(),
        Some("sim BOSS_SIM_ENABLED=unset (no tenant manifest to derive from)"),
        "{out}"
    );
}

#[test]
fn the_cluster_manifest_carries_neither_half_of_the_sims_loopback_pair() {
    // The pair is the sim engine's wiring, derived by the launcher when
    // the sim runs; in boss.yaml it was dead config on every instance
    // whose sim is parked or whose tenant has no engine.
    let yaml = std::fs::read_to_string(repo_root().join(MANIFEST)).unwrap();
    for key in ["BOSS_EVENT_WEBHOOK_URL", "BOSS_SIM_CALLBACK_BIND"] {
        let hits: Vec<&str> = yaml.lines().filter(|l| l.contains(key)).collect();
        assert!(hits.is_empty(), "{MANIFEST} still carries {key}: {hits:?}");
    }
}

#[test]
fn the_image_build_warns_rather_than_fails_without_the_brewerys_engine() {
    let df = std::fs::read_to_string(repo_root().join(DOCKERFILE)).unwrap();
    let fatal: Vec<&str> = df
        .lines()
        .filter(|l| l.contains("FATAL") && l.contains("boss-brewery-sim"))
        .collect();
    assert!(
        fatal.is_empty(),
        "the brewery's engine is a tenant binary, not the platform's: {fatal:?}"
    );
    assert!(
        df.lines()
            .any(|l| l.contains("WARN") && l.contains("boss-brewery-sim")),
        "its absence is still said out loud"
    );
    // The platform's gateway is still the build's hard requirement.
    assert!(
        df.contains("test -x /out/boss-gateway || (echo \"FATAL: boss-gateway missing in /out\"")
    );
}

#[test]
fn the_image_carries_the_module_reader_beside_the_launcher() {
    // Pinned the way tenant-launch.sh is (infra/lint/the-image-carries-
    // what-the-launcher-sources.sh reads the SOURCED list; this names
    // the file so a rename cannot slip past as "not sourced").
    let df = std::fs::read_to_string(repo_root().join(DOCKERFILE)).unwrap();
    assert!(
        df.contains("COPY infra/oss-quickstart/tenant-modules.sh /usr/local/bin/tenant-modules.sh"),
        "the Dockerfile must COPY tenant-modules.sh beside boss-launch"
    );
    let launcher = std::fs::read_to_string(repo_root().join(LAUNCHER)).unwrap();
    assert!(
        launcher.contains("SOURCED=(tenant-launch.sh tenant-modules.sh)"),
        "the launcher sources it"
    );
}
