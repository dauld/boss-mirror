//! `boss deploy` — build, install, and restart services.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Workspace root — three levels up from crates/orchestrators/boss-cli/.
fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent() // crates/orchestrators/
        .and_then(|p| p.parent()) // crates/
        .and_then(|p| p.parent()) // repo root
        .unwrap_or(Path::new("."))
        .to_path_buf()
}

#[derive(Debug)]
struct ServiceDef {
    /// CLI-facing name (e.g. "assets")
    name: &'static str,
    /// Cargo package name
    package: &'static str,
    /// Binary name produced by cargo
    binary: &'static str,
    /// Extra cargo features
    features: &'static [&'static str],
    /// systemd unit name (None = no service to restart)
    unit: Option<&'static str>,
}

const SERVICES: &[ServiceDef] = &[
    ServiceDef {
        name: "assets",
        package: "boss-assets",
        binary: "boss-assets-api",
        features: &["postgres"],
        unit: Some("boss-assets-api"),
    },
    ServiceDef {
        name: "catalog",
        package: "boss-catalog",
        binary: "boss-catalog-api",
        features: &["postgres"],
        unit: Some("boss-catalog-api"),
    },
    ServiceDef {
        name: "commerce",
        package: "boss-commerce",
        binary: "boss-commerce-api",
        features: &["postgres"],
        unit: Some("boss-commerce-api"),
    },
    ServiceDef {
        name: "people",
        package: "boss-people",
        binary: "boss-people-api",
        features: &["postgres"],
        unit: Some("boss-people-api"),
    },
    ServiceDef {
        name: "inventory",
        package: "boss-inventory",
        binary: "boss-inventory-api",
        features: &["postgres"],
        unit: Some("boss-inventory-api"),
    },
    ServiceDef {
        name: "messages",
        package: "boss-messages",
        binary: "boss-messages-api",
        features: &["postgres"],
        unit: Some("boss-messages-api"),
    },
    ServiceDef {
        name: "shipping",
        package: "boss-shipping",
        binary: "boss-shipping-api",
        features: &["postgres"],
        unit: Some("boss-shipping-api"),
    },
    ServiceDef {
        name: "gateway",
        package: "boss-gateway",
        binary: "boss-gateway",
        features: &[],
        unit: Some("boss-gateway"),
    },
    ServiceDef {
        name: "cybernetics",
        package: "boss-cybernetics",
        binary: "boss-cybernetics",
        features: &[],
        unit: Some("boss-cybernetics"),
    },
    ServiceDef {
        name: "observability",
        package: "boss-observability",
        binary: "boss-observability",
        features: &[],
        unit: Some("boss-observability"),
    },
    ServiceDef {
        name: "sim",
        package: "boss-sim",
        binary: "boss-sim",
        features: &["http", "nats"],
        unit: None,
    },
    ServiceDef {
        name: "simapi",
        package: "boss-sim-api",
        binary: "boss-sim-api",
        features: &[],
        unit: Some("boss-sim-api"),
    },
    ServiceDef {
        name: "ml",
        package: "boss-ml",
        binary: "boss-ml-api",
        features: &["postgres"],
        unit: Some("boss-ml-api"),
    },
    ServiceDef {
        name: "ledger",
        package: "boss-ledger",
        binary: "boss-ledger-api",
        features: &["postgres"],
        unit: Some("boss-ledger-api"),
    },
    ServiceDef {
        name: "policy",
        package: "boss-policy",
        binary: "boss-policy-api",
        features: &["postgres"],
        unit: Some("boss-policy-api"),
    },
    ServiceDef {
        name: "content",
        package: "boss-content",
        binary: "boss-content-api",
        features: &["postgres"],
        unit: Some("boss-content-api"),
    },
];

pub async fn list() -> Result<()> {
    println!(
        "{:<16} {:<20} {:<24} FEATURES",
        "SERVICE", "PACKAGE", "BINARY"
    );
    println!("{}", "-".repeat(76));
    for s in SERVICES {
        let features = if s.features.is_empty() {
            "—".to_string()
        } else {
            s.features.join(", ")
        };
        println!(
            "{:<16} {:<20} {:<24} {}",
            s.name, s.package, s.binary, features
        );
    }
    println!("\n{} services registered.", SERVICES.len());
    Ok(())
}

pub async fn run(service: Option<&str>, skip_build: bool) -> Result<()> {
    let targets: Vec<&ServiceDef> = match service {
        Some(name) => {
            let def = SERVICES.iter().find(|s| s.name == name).ok_or_else(|| {
                let names: Vec<&str> = SERVICES.iter().map(|s| s.name).collect();
                anyhow::anyhow!("unknown service '{name}'. Available: {}", names.join(", "))
            })?;
            vec![def]
        }
        None => SERVICES.iter().collect(),
    };

    for def in &targets {
        deploy_one(def, skip_build)?;
    }

    if targets.len() > 1 {
        println!("\nDeployed {} services.", targets.len());
    }

    Ok(())
}

fn deploy_one(def: &ServiceDef, skip_build: bool) -> Result<()> {
    let root = workspace_root();
    println!("--- {} ---", def.name);

    // Build
    if !skip_build {
        print!("  building {} (release)...", def.package);
        let mut args = vec!["build", "--release", "-p", def.package];
        let features_str;
        if !def.features.is_empty() {
            args.push("--features");
            features_str = def.features.join(",");
            args.push(&features_str);
        }

        let status = Command::new("cargo")
            .args(&args)
            .current_dir(&root)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .status()
            .with_context(|| format!("running cargo build for {}", def.package))?;

        if !status.success() {
            anyhow::bail!("cargo build failed for {}", def.package);
        }
        println!(" ok");
    }

    // Install binary
    let src = root.join(format!("target/release/{}", def.binary));
    let dst = format!("/usr/local/bin/{}", def.binary);

    if !src.exists() {
        anyhow::bail!(
            "binary not found at {} — did the build succeed?",
            src.display()
        );
    }

    let src_str = src.to_string_lossy();
    print!("  installing {dst}...");
    let status = Command::new("sudo")
        .args(["install", "-m", "0755", &*src_str, &dst])
        .status()
        .with_context(|| format!("installing {}", def.binary))?;
    if !status.success() {
        anyhow::bail!("install failed for {}", def.binary);
    }
    println!(" ok");

    // Restart service
    if let Some(unit) = def.unit {
        let unit_name = format!("{unit}.service");
        print!("  restarting {unit_name}...");
        let status = Command::new("sudo")
            .args(["systemctl", "restart", &unit_name])
            .status()
            .with_context(|| format!("restarting {unit_name}"))?;
        if !status.success() {
            println!(" FAILED (service may not be enabled)");
        } else {
            println!(" ok");
        }
    } else {
        println!("  (no systemd service)");
    }

    Ok(())
}

pub async fn clean() -> Result<()> {
    let root = workspace_root();
    println!("Cleaning debug build artifacts...");
    let status = Command::new("cargo")
        .args(["clean", "--profile", "dev"])
        .current_dir(&root)
        .status()
        .context("running cargo clean")?;
    if !status.success() {
        anyhow::bail!("cargo clean failed");
    }
    println!("Done. Release artifacts preserved.");
    Ok(())
}

pub async fn deploy_web() -> Result<()> {
    let root = workspace_root();
    println!("--- web frontend ---");

    // Build
    print!("  building frontend...");
    let status = Command::new("bun")
        .args(["run", "build"])
        .current_dir(root.join("apps/web"))
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .status()
        .context("running bun build")?;
    if !status.success() {
        anyhow::bail!("frontend build failed");
    }
    println!(" ok");

    // Copy to the gateway's static dir — where the gateway serves
    // the SPA from.
    let static_dir = "/var/lib/boss-web/dist";
    if std::path::Path::new(static_dir).exists() {
        let dist_src = root.join("apps/web/dist/.");
        print!("  copying dist/ to {static_dir}...");
        // Wipe first — otherwise stale chunk-*.js files accumulate
        // and index.html references can miss on a page reload.
        let _ = Command::new("sudo")
            .args(["sh", "-c", &format!("rm -f {static_dir}/chunk-*")])
            .status();
        let status = Command::new("sudo")
            .args(["cp", "-r", &dist_src.to_string_lossy(), static_dir])
            .status()
            .context("copying web dist")?;
        if !status.success() {
            anyhow::bail!("copy failed");
        }
        println!(" ok");
    } else {
        println!("  {static_dir} does not exist — skipping copy");
        println!("  (built files are in apps/web/dist/)");
    }

    Ok(())
}
