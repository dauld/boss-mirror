//! [`prepare_model`] — the brewery's single tenant-prepare entry
//! point. Against a running BOSS stack — each service on its own port
//! by default, or all behind one gateway URL — it seeds the entire
//! brewery tenant model through the public API, in dependency order:
//!
//! 1. classes — POST /api/classes/batch (the taxonomy that employee +
//!    account writes validate against, so it lands first).
//! 2. policy — tenant role grants ([`boss_policy::bootstrap`]); these are
//!    capability-level (`resource = "workflow"`, not a specific kind), so
//!    they need no published Workflows, and the design-Job approval in
//!    step 4 needs the `workflow-approver` grant resolved first.
//! 3. data — operators, employees, accounts, vendors, messages,
//!    finished-goods, raw materials, equipment, assets, and opening
//!    balances ([`super::seed_tenant_data`]).
//! 4. Workflows LAST ([`super::publish_workflows`]) — each one opens a real
//!    `workflow-design` Job with role-bearing `approve` + `publish` steps
//!    that the dispatcher auto-assigns the moment they go ready. Their
//!    holders (the `workflow-approver`-granted leaders, it-director,
//!    platform-admin) must already be seeded AND queryable, or the
//!    assignment dead-letters against a cold roster — so we barrier on the
//!    people projection before opening them.
//!
//! This collapses what reset-to-baseline / seed-brewery-tenant.sh
//! drove as four scattered binary + curl steps into one library call,
//! so the offline regen, the live demo, and CI all run identical code.
//!
//! Two adjacent concerns stay with the caller by design:
//!
//! - **Clock loop-bounds.** [`super::seed_tenant_data`] rebases the
//!   clock to the sim epoch internally (the pre-sim provisioning
//!   window) so seed writes stamp at day 0; the epoch_end + warp_factor
//!   that bound the sim's run are set by whoever drives the clock
//!   (reset-to-baseline / the sim daemon), not here.
//! - **Platform baseline.** The platform operator-baseline
//!   (`bootstrap-admin`, minted from a deploy credentials file) and
//!   the projection rebuilds are platform-init / read-model concerns —
//!   not tenant data, and not API-orchestratable — so they remain in
//!   the deploy/reset orchestration.

use std::path::Path;

use anyhow::{Context, Result};
use reqwest::blocking::Client;
use tracing::info;

use super::{SeedBases, publish_workflows, seed_tenant_data};

/// Seed the entire brewery tenant model through the public API.
///
/// `gateway_base` selects the routing: `None` sends each service to
/// its own localhost port (`boss_ports` defaults) — the reset /
/// quickstart path, which seeds with the gateway stopped. `Some(url)`
/// routes every `/api/*` prefix through one gateway URL (a deployment
/// whose gateway is up during seeding). `seeds_dir` is the brewery
/// seed bundle (`examples/brewery/seeds`). Idempotent throughout —
/// safe to re-run.
pub fn prepare_model(gateway_base: Option<&str>, seeds_dir: &Path) -> Result<()> {
    // classes / jobs / policy each take a single base; the tenant-data
    // seeder takes the full per-service map. A gateway override points
    // all of them at one URL; otherwise each resolves to its own port.
    let classes_base = gateway_base
        .map(str::to_string)
        .unwrap_or_else(|| boss_ports::url("classes"));
    let calendar_base = gateway_base
        .map(str::to_string)
        .unwrap_or_else(|| boss_ports::url("calendar"));
    let jobs_base = gateway_base
        .map(str::to_string)
        .unwrap_or_else(|| boss_ports::url("jobs"));
    let policy_base = gateway_base
        .map(str::to_string)
        .unwrap_or_else(|| boss_ports::url("policy"));
    let people_base = gateway_base
        .map(str::to_string)
        .unwrap_or_else(|| boss_ports::url("people"));
    let data_bases = match gateway_base {
        Some(g) => SeedBases::all(g),
        None => SeedBases::from_ports(),
    };

    info!(
        gateway = ?gateway_base,
        seeds = %seeds_dir.display(),
        "preparing brewery tenant model"
    );

    // 1. Classes first — employee role + account-type writes validate
    //    against the Class registry.
    seed_classes(&classes_base, seeds_dir)?;

    // 1b. Business calendars — reference data (banking/tax holidays) the
    //     dispatcher's timing triggers and the simulator resolve business
    //     days from. Like classes: load before anything that consumes them.
    seed_business_calendars(&calendar_base, seeds_dir)?;

    // 1c. The tenant's own identity — Q6: the organization being
    //     modeled is itself a Subject, one row per tenant. The
    //     org-level Workflows (payroll, tax filings, AP runs, facility
    //     overhead, the production heartbeat) open their Jobs about
    //     it, so the identity must exist before the periodic engine
    //     opens the first one. Hard-fail: a missing company identity
    //     starves every org-level Job at the existence gate.
    let subjects_base = gateway_base
        .map(str::to_string)
        .unwrap_or_else(|| boss_ports::url("subject-kinds"));
    let tenant = boss_sim::shape_driven::TenantConfig::load(&seeds_dir.join("tenant.toml"))
        .context("loading tenant.toml for the company identity")?;
    crate::mint_subject_identity(
        "company",
        &tenant.meta.tenant_id,
        Some(&tenant.meta.display_name),
        &subjects_base,
    )
    .map_err(|e| anyhow::anyhow!("minting the company identity: {e}"))?;
    info!(company = %tenant.meta.tenant_id, "company identity minted");

    // 2. Tenant policy grants — core ships only platform rules; the
    //    brewery org chart's row-level access matrix arrives here. These
    //    grants are capability-level (`resource = "workflow"`, not a
    //    specific published kind), so they don't depend on the Workflow
    //    registry being populated — and the design-Job approval in step 4
    //    needs the `workflow-approver` grant to resolve to its
    //    operational-leader holders.
    boss_policy::bootstrap::publish_policy_rules(
        &policy_base,
        &seeds_dir.join("policy_rules.toml"),
        false,
        "brewery-policy-bootstrap",
        None,
    )?;

    // 3. Tenant data — operators, employees, accounts, vendors,
    //    messages, finished-goods, raw materials, equipment, assets,
    //    + opening balances. None of it opens Jobs (so it needs no
    //    Workflows yet); it DOES seed the workforce step 4 depends on.
    seed_tenant_data(&data_bases, seeds_dir, None)?;

    // 4. Workflows LAST — publishing each brewery Workflow opens a real
    //    `workflow-design` Job whose `approve` (sign-off, authority
    //    `workflow-approver`) and `publish` (it-director / platform-admin)
    //    steps are role-bearing. The dispatcher auto-assigns role-bearing
    //    steps the instant they go ready, so those holders must already be
    //    seeded AND queryable — otherwise the assignment NAKs against an
    //    empty roster and dead-letters (the prepare flow still completes
    //    the steps directly, but the dispatcher's parallel attempt is what
    //    exhausts its redelivery budget). Barrier on the people projection
    //    first, then open the design Jobs. dev=true auto-walks the sign-off
    //    (unattended seed, same as `boss-brewery-bootstrap --dev`);
    //    publish_workflows takes the workflows.toml FILE (not the dir).
    wait_for_people_projection(&people_base)?;
    publish_workflows(
        &jobs_base,
        &seeds_dir.join("workflows.toml"),
        "brewery-bootstrap",
        true,
        false,
        None,
    )?;

    info!("brewery tenant model prepared");
    Ok(())
}

/// Block until the people read-model reflects the just-seeded workforce,
/// so the role-bearing steps opened immediately after (the
/// `workflow-design` Jobs) can be assigned to real holders instead of
/// dead-lettering against a cold roster. Polls `/api/people` until the
/// count is non-trivial and stable across three reads (the hire backlog
/// has drained), or 90s elapse. Mirrors the sim-readiness barrier in
/// `validate-brewery-sim.sh`, but inside the shared prepare path so every
/// caller (offline regen, live demo, CI) gets it. Best-effort: a timeout
/// logs and proceeds rather than aborting the seed.
fn wait_for_people_projection(people_base: &str) -> Result<()> {
    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()?;
    let url = format!("{}/api/people", people_base.trim_end_matches('/'));
    let (mut prev, mut stable) = (0usize, 0u32);
    for _ in 0..90 {
        let count = client
            .get(&url)
            .send()
            .ok()
            .and_then(|r| r.json::<serde_json::Value>().ok())
            .and_then(|v| v.as_array().map(|a| a.len()))
            .unwrap_or(0);
        if count > 100 && count == prev {
            stable += 1;
            if stable >= 3 {
                info!(people = count, "roster ready — opening design Jobs");
                return Ok(());
            }
        } else {
            stable = 0;
        }
        prev = count;
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
    info!(
        people = prev,
        "people projection did not stabilize in 90s; opening design Jobs anyway"
    );
    Ok(())
}

/// POST the brewery's Class registry (`seeds/classes.json`) to
/// `/api/classes/batch`. Classes are the taxonomy employee + account
/// writes validate against, so they land before any of those.
///
/// `x-sim-origin: true` lets the batch land as seed-origin data
/// (matching the reset-to-baseline curl); the seed-loader identity
/// carries platform-admin provenance.
fn seed_classes(api_base: &str, seeds_dir: &Path) -> Result<()> {
    let path = seeds_dir.join("classes.json");
    let body =
        std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;

    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()?;
    let url = format!("{}/api/classes/batch", api_base.trim_end_matches('/'));
    let resp = client
        .post(&url)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .header("x-sim-origin", "true")
        .header(
            "x-boss-user",
            r#"{"id":"automation:classes-seed","role":"platform-admin","access_tier":"operator","territory_account_ids":[],"direct_report_ids":[],"department":"platform"}"#,
        )
        .body(body)
        .send()
        .with_context(|| format!("POST {url}"))?;
    let status = resp.status();
    if !status.is_success() {
        anyhow::bail!("POST {url} → {status} {}", resp.text().unwrap_or_default());
    }
    info!(path = %path.display(), "brewery classes seeded");
    Ok(())
}

/// POST the brewery's business calendars (`seeds/business_calendars.json`)
/// to `/api/calendar/business-calendars/batch`. These are the banking +
/// tax calendars the dispatcher's timing triggers and the simulator
/// resolve business days from — DATA, not hardcoded Rust. Same
/// seed-origin provenance as the Class registry.
fn seed_business_calendars(api_base: &str, seeds_dir: &Path) -> Result<()> {
    let path = seeds_dir.join("business_calendars.json");
    let body =
        std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;

    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()?;
    let url = format!(
        "{}/api/calendar/business-calendars/batch",
        api_base.trim_end_matches('/')
    );
    let resp = client
        .post(&url)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .header("x-sim-origin", "true")
        .header(
            "x-boss-user",
            r#"{"id":"automation:calendar-seed","role":"platform-admin","access_tier":"operator","territory_account_ids":[],"direct_report_ids":[],"department":"platform"}"#,
        )
        .body(body)
        .send()
        .with_context(|| format!("POST {url}"))?;
    let status = resp.status();
    if !status.is_success() {
        anyhow::bail!("POST {url} → {status} {}", resp.text().unwrap_or_default());
    }
    info!(path = %path.display(), "brewery business calendars seeded");
    Ok(())
}
