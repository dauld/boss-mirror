//! `boss estate declare <estate.toml> [--gateway <url>] [--dry-run]` —
//! the tree's estate declaration, published (backlog ee368d0c; design
//! 42277636 first wave, audit H5).
//!
//! WHY. Until 2026-09-18 the estate registry — `nodes`, `node_roles`
//! — was written only by schema migrations, so every fresh database
//! BOSS ever created, an adopter's included, booted declaring this
//! LAN's seven machines and their roles. The estate is instance data:
//! it belongs to the tree that converges the instance
//! (infra/estate/estate.toml, the one file the estate's addresses live
//! in since H10), and it reaches a database the way every other
//! declaration does — through a door, insert-if-absent, on every pod
//! start. This verb is that publish; the launcher runs it before the
//! tenant (infra/seed-estate.sh), and a fresh database declares nothing
//! until it has run.
//!
//! WHAT IT SENDS. The file's `[[node]]` rows, read with the one loader
//! the tests read them with (`boss_jobs::estate_seed`), to `POST
//! /api/estate/nodes/batch` signed as `automation:estate-seed` at
//! operator tier — the door refuses any lesser caller. The outcome line
//! says what landed: a node already there is kept as it is, a role not
//! yet on it is added, and a second run inserts nothing.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

/// The x-boss-user identity the declaration carries — a dedicated
/// automation, so the `node.declared` facts name the launcher's
/// publish and never a person or the tenant seed.
pub const ESTATE_SEED_USER: &str = r#"{"id":"automation:estate-seed","role":"platform-admin","access_tier":"operator","territory_account_ids":[],"direct_report_ids":[],"department":"platform"}"#;

#[derive(clap::Subcommand)]
pub enum Cmd {
    /// The estate — the machines this instance declares it runs on.
    #[command(subcommand)]
    Estate(EstateAction),
}

#[derive(clap::Subcommand)]
pub enum EstateAction {
    /// Publish the tree's node declarations (the `[[node]]` rows of
    /// infra/estate/estate.toml) to the estate registry, insert-if-absent.
    Declare {
        /// The estate file (infra/estate/estate.toml).
        source: PathBuf,
        /// Route the write through one gateway URL (default: the jobs
        /// service's own localhost port, the in-pod launcher path).
        #[arg(long)]
        gateway: Option<String>,
        /// Print what would be sent and send nothing.
        #[arg(long)]
        dry_run: bool,
    },
}

/// What one run would send: the rows and the door, for the plan line.
pub fn plan(source: &Path) -> Result<Vec<boss_jobs::port::EstateNodeInput>> {
    boss_jobs::estate_seed::load_estate_toml(source)
        .map_err(|e| anyhow::anyhow!("{}: {e}", source.display()))
}

pub fn declare(source: &Path, gateway: Option<&str>, dry_run: bool) -> Result<String> {
    let rows = plan(source)?;
    if rows.is_empty() {
        bail!(
            "{} declares no [[node]] rows — an estate with no machines is a file that lost its rows, not an empty estate",
            source.display()
        );
    }
    let base = gateway
        .map(|g| g.trim_end_matches('/').to_string())
        .unwrap_or_else(|| boss_ports::url("jobs"));
    let url = format!("{base}/api/estate/nodes/batch");
    let ids: Vec<&str> = rows.iter().map(|r| r.id.as_str()).collect();
    let head = format!(
        "estate: {} nodes from {} ({}) → POST {url}",
        rows.len(),
        source.display(),
        ids.join(", ")
    );
    if dry_run {
        return Ok(format!("{head}\n  dry run: nothing sent"));
    }
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        "x-boss-user",
        reqwest::header::HeaderValue::from_static(ESTATE_SEED_USER),
    );
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .default_headers(headers)
        .build()?;
    let resp = client
        .post(&url)
        .json(&boss_jobs::port::EstateNodeBatch { nodes: rows })
        .send()
        .with_context(|| format!("POST {url}"))?;
    let status = resp.status();
    if !status.is_success() {
        bail!("POST {url} → {status} {}", resp.text().unwrap_or_default());
    }
    let out: boss_jobs::port::EstateBatchOutcome = resp
        .json()
        .with_context(|| format!("POST {url}: the outcome did not parse"))?;
    Ok(format!(
        "{head}\n  received {}, inserted {}, roles inserted {} (a node already there is kept)",
        out.received, out.inserted, out.roles_inserted
    ))
}

pub fn dispatch(cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::Estate(EstateAction::Declare {
            source,
            gateway,
            dry_run,
        }) => {
            println!("{}", declare(&source, gateway.as_deref(), dry_run)?);
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use boss_testing::scratch::{scratch_dir, write_file};

    const FILE: &str = "sor_url = \"http://192.0.2.34:7900\"\n\n[[node]]\nid = \"cp-1\"\n\
                        label = \"cp-1\"\naddress = \"192.0.2.11\"\nrole = \"talos-control-plane\"\n\
                        [[node]]\nid = \"forge\"\nlabel = \"Forge\"\naddress = \"192.0.2.15\"\n\
                        role = \"forge\"\nroles = [\"cluster-operator\"]\n";

    /// The tree's own file is what the launcher publishes: it parses,
    /// declares at least the machines the converges read roles for,
    /// and a dry run names them without sending.
    #[test]
    fn the_trees_estate_file_is_a_publishable_declaration() {
        let source = boss_testing::repo_root().join("infra/estate/estate.toml");
        let rows = plan(&source).expect("the tree's estate parses");
        let ids: Vec<&str> = rows.iter().map(|r| r.id.as_str()).collect();
        for host in ["forge", "boss-gcp"] {
            assert!(
                ids.contains(&host),
                "{host} reads its roles off the registry (node-roles.sh), so the tree must declare it: {ids:?}"
            );
        }
        let line = declare(&source, None, true).unwrap();
        assert!(line.contains("dry run") && line.contains("forge"), "{line}");
    }

    #[test]
    fn a_file_with_no_nodes_is_refused_not_sent_as_an_empty_estate() {
        let dir = scratch_dir("boss-cli-estate-declare");
        let f = dir.join("estate.toml");
        write_file(&f, "sor_url = \"http://192.0.2.34:7900\"\n");
        let why = declare(&f, None, true).unwrap_err().to_string();
        assert!(why.contains("no [[node]] rows"), "{why}");
        write_file(&f, FILE);
        let line = declare(&f, Some("http://127.0.0.1:1/"), true).unwrap();
        assert!(
            line.contains("http://127.0.0.1:1/api/estate/nodes/batch")
                && line.contains("cp-1, forge"),
            "{line}"
        );
    }

    /// The identity is a dedicated automation at operator tier — the
    /// door's requirement — and deserialises as the gateway's User.
    #[test]
    fn the_seed_identity_is_an_operator_automation() {
        let u: boss_policy_client::User = serde_json::from_str(ESTATE_SEED_USER).unwrap();
        assert_eq!(u.id, "automation:estate-seed");
        assert_eq!(u.access_tier, boss_policy_client::AccessTier::Operator);
    }
}
