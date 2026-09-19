//! A tenant's sales-tax rates live ONCE in the tree: in that tenant's
//! `seeds/tax.toml`, read by the ledger's loader
//! (`boss_ledger::tax_registry::load_tax_toml`), published through
//! `POST /api/ledger/tax/batch`, and answered by
//! `GET /api/ledger/sales-tax-rates` (backlog fc27a0ce; 7f163e58 moved
//! them there, 2026-09-18). No crate bundles a copy.
//!
//! WHAT WAS MEASURED (2026-09-19). The 27 US rates lived three times:
//! `crates/orchestrators/boss-sim/data/us_sales_tax_rates.toml`, bundled
//! into the binary by `boss_sim::tax_rates` and calling the
//! migration "a downstream copy kept aligned by hand"; the migration
//! `40-ledger.sql` (append-only history — left alone); and the brewery's
//! `seeds/tax.toml`. The sim's copy had NO reader: `rate_for_state` and
//! `compute_tax` were called from nowhere outside their own tests, and
//! the `boss-ledger-batch` SalesTaxFilingRule its doc named does not
//! exist. The ledger computes tax per invoice from the table the tenant
//! published; the sim never did. A dead copy of a fact is the cheapest
//! kind to collapse (CLAUDE.md §9a: collapse if you can; deletion is
//! the goal), so the module and its bundle are gone and this pins that
//! no crate grows one back.

use boss_testing::repo_root;
use std::path::{Path, PathBuf};

/// Every source and data file under `crates/`, skipping build output.
fn crate_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let p = entry.path();
        if p.is_dir() {
            if p.file_name().is_some_and(|n| n == "target") {
                continue;
            }
            crate_files(&p, out);
        } else {
            out.push(p);
        }
    }
}

#[test]
fn no_crate_bundles_a_sales_tax_rate_table() {
    let root = repo_root();
    let mut files = Vec::new();
    crate_files(&root.join("crates"), &mut files);
    assert!(files.len() > 100, "the walker read the crates tree");

    let named: Vec<_> = files
        .iter()
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.contains("sales_tax") || n.contains("tax_rates"))
        })
        .map(|p| p.strip_prefix(&root).unwrap_or(p).display().to_string())
        .collect();
    assert!(
        named.is_empty(),
        "a crate carries a sales-tax rate file — the tenant's seeds/tax.toml is the one definition: {named:?}"
    );

    let bundled: Vec<_> = files
        .iter()
        .filter(|p| p.extension().is_some_and(|e| e == "rs"))
        .filter_map(|p| {
            let text = std::fs::read_to_string(p).ok()?;
            // A rate TABLE, specifically: boss-ledger bundles a kind ->
            // liability-account map (`tax_liability_accounts.toml`), a
            // different fact with its own history.
            // (The macro name is assembled so this file's own line
            // does not match itself.)
            let bundle = format!("include_{}!", "str");
            let hit = text.lines().find(|l| {
                l.contains(&bundle) && (l.contains("sales_tax") || l.contains("tax_rate"))
            })?;
            Some(format!(
                "{}: {}",
                p.strip_prefix(&root).unwrap_or(p).display(),
                hit.trim()
            ))
        })
        .collect();
    assert!(
        bundled.is_empty(),
        "a crate bundles a tax table by include_str! — a tenant's regime is tenant data, published from its seed: {bundled:?}"
    );
}
