//! The used-device-shop's unified **prepare** phase — seed the whole
//! tenant model through the public API, idempotently. Sister of
//! `boss_brewery_engine::prepare`, and deliberately the same shape:
//! one [`prepare_model`] entry point, called by the
//! `boss-used-device-shop-engine prepare` binary (which
//! `infra/bootstrap-vm.sh`'s `TENANT=device-shop` branch drives), so
//! a fresh-VM install and any future regen path run identical code.
//!
//! Dependency order, mirroring the brewery's `prepare_model`:
//!
//! 1. classes — POST `seeds/classes.toml` to /api/classes/batch. The
//!    platform registry (infra/postgres/schema/01-registries.sql)
//!    already ships this tenant's departments, employment types,
//!    statuses, and all but one role; the seed file carries the
//!    tenant-only taxonomy rows (asset categories, document kinds,
//!    account-team roles, the `auditor` role). Lands first because
//!    employee + catalog writes validate against it.
//! 2. company identity — the organization being modeled is itself a
//!    Subject (one row per tenant, id = tenant.toml's
//!    `meta.tenant_id`); org-level Workflows open Jobs about it.
//! 3. policy — tenant role grants via
//!    [`boss_policy::bootstrap::publish_policy_rules`], the same
//!    shared impl the `boss-policy-bootstrap` binary and the
//!    brewery's prepare drive.
//! 4. employees — the 76-person roster in `data/employees.json` via
//!    POST /api/people (two passes: create, then link managers).
//!    Must land before Workflows publish so the dispatcher's
//!    role-bearing auto-assignment resolves against a real roster.
//! 5. catalog — the device models in `data/catalog.json` via
//!    POST /api/catalog/models (after classes: model `category`
//!    validates against the asset-category Class rows).
//! 6. Workflows LAST — [`boss_jobs::bootstrap::publish_workflows`],
//!    the shared insert-if-missing door the brewery publishes
//!    through, after a barrier on the people projection.
//!
//! All steps are idempotent (insert-if-absent batch upserts, POST →
//! 409 swallow, provenance-checked Workflow publishes), so a re-run
//! after a partial failure resumes cleanly.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use reqwest::blocking::Client;
use serde_json::json;
use tracing::{info, warn};

/// The x-boss-user identity every seed write carries. A dedicated
/// seed-loader identity, not a person: "the used-device-shop seed
/// bundle landed these rows" is the correct provenance (same
/// reasoning as the brewery's `automation:brewery-seed`).
const SEED_USER: &str = r#"{"id":"automation:used-device-shop-seed","role":"platform-admin","access_tier":"operator","territory_account_ids":[],"direct_report_ids":[],"department":"platform"}"#;

/// Seed the entire used-device-shop tenant model through the public
/// API.
///
/// `gateway_base` selects the routing: `None` sends each service to
/// its own localhost port (`boss_ports` defaults) — the fresh-VM
/// install path. `Some(url)` routes every `/api/*` prefix through one
/// gateway URL. `seeds_dir` is the tenant seed bundle
/// (`examples/used-device-shop/seeds`); the roster + catalog data
/// live beside it at `<seeds_dir>/../data`. Idempotent throughout —
/// safe to re-run.
pub fn prepare_model(gateway_base: Option<&str>, seeds_dir: &Path) -> Result<()> {
    let resolve = |service: &str| {
        gateway_base
            .map(str::to_string)
            .unwrap_or_else(|| boss_ports::url(service))
    };
    let classes_base = resolve("classes");
    let subjects_base = resolve("subject-kinds");
    let policy_base = resolve("policy");
    let people_base = resolve("people");
    let catalog_base = resolve("catalog");
    let jobs_base = resolve("jobs");

    info!(
        gateway = ?gateway_base,
        seeds = %seeds_dir.display(),
        "preparing used-device-shop tenant model"
    );

    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()?;
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        "x-boss-user",
        reqwest::header::HeaderValue::from_static(SEED_USER),
    );
    headers.insert(
        "x-sim-origin",
        reqwest::header::HeaderValue::from_static("true"),
    );

    // 1. Classes first — employee role + catalog category writes
    //    validate against the Class registry.
    seed_classes(&client, &classes_base, seeds_dir)?;

    // 2. The tenant's own identity — the organization being modeled
    //    is itself a Subject; org-level Workflows open Jobs about it.
    //    Hard-fail: a missing company identity starves every
    //    org-level Job at the existence gate.
    let tenant = boss_sim::shape_driven::TenantConfig::load(&seeds_dir.join("tenant.toml"))
        .context("loading tenant.toml for the company identity")?;
    mint_company_identity(
        &client,
        &subjects_base,
        &tenant.meta.tenant_id,
        &tenant.meta.display_name,
    )?;
    info!(company = %tenant.meta.tenant_id, "company identity minted");

    // 3. Tenant policy grants — core ships only platform rules; the
    //    shop's org-chart access matrix arrives here. These grants
    //    are capability-level, so they don't depend on the Workflow
    //    registry — and the design-Job approvals in step 6 need the
    //    `workflow-approver` grant resolved first.
    boss_policy::bootstrap::publish_policy_rules(
        &policy_base,
        &seeds_dir.join("policy_rules.toml"),
        false,
        "used-device-shop-policy-bootstrap",
        None,
    )?;

    // 4. The roster. Employees land before Workflows so the
    //    dispatcher's role-bearing step auto-assignment resolves
    //    against real people instead of dead-lettering.
    let roster_len = seed_employees(&client, &people_base, &headers, seeds_dir)?;

    // 5. Device catalog — the Equipment KB models this tenant
    //    refurbishes. After classes (category validation).
    seed_catalog_models(&client, &catalog_base, &headers, seeds_dir)?;

    // 6. Workflows LAST — same reasoning as the brewery: publishing
    //    opens real `workflow-design` Jobs with role-bearing steps,
    //    so barrier on the people projection first. dev=true
    //    auto-walks the sign-off (unattended seed).
    wait_for_people_projection(&client, &people_base, roster_len);
    boss_jobs::bootstrap::publish_workflows(
        &jobs_base,
        &seeds_dir.join("workflows.toml"),
        &tenant.meta.tenant_id,
        true,
        false,
        None,
    )?;

    info!("used-device-shop tenant model prepared");
    Ok(())
}

/// Where the roster + catalog data live: `<seeds_dir>/../data` — the
/// same convention `UsedDeviceShopEngineState::load` uses.
fn data_dir(seeds_dir: &Path) -> PathBuf {
    seeds_dir.join("..").join("data")
}

/// Parse `seeds/classes.toml` (`[[class]]` rows) into the JSON rows
/// `POST /api/classes/batch` accepts. The TOML row schema mirrors
/// the endpoint's `ClassInput` (subject_kind / code / display_name /
/// member_attribute / metadata / sort_order), so this is a straight
/// format translation, no re-shaping.
fn load_class_rows(path: &Path) -> Result<Vec<serde_json::Value>> {
    let raw = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    #[derive(serde::Deserialize)]
    struct Bundle {
        class: Vec<serde_json::Value>,
    }
    let bundle: Bundle =
        toml::from_str(&raw).with_context(|| format!("parsing {}", path.display()))?;
    Ok(bundle.class)
}

/// POST the tenant's Class rows to `/api/classes/batch` — the same
/// insert-if-absent door the brewery's seed_classes uses; only the
/// on-disk format differs (this tenant authors TOML).
fn seed_classes(client: &Client, api_base: &str, seeds_dir: &Path) -> Result<()> {
    let path = seeds_dir.join("classes.toml");
    let rows = load_class_rows(&path)?;
    let count = rows.len();

    let url = format!("{}/api/classes/batch", api_base.trim_end_matches('/'));
    let resp = client
        .post(&url)
        .header("x-sim-origin", "true")
        .header("x-boss-user", SEED_USER)
        .json(&rows)
        .send()
        .with_context(|| format!("POST {url}"))?;
    let status = resp.status();
    if !status.is_success() {
        anyhow::bail!("POST {url} → {status} {}", resp.text().unwrap_or_default());
    }
    info!(path = %path.display(), count, "used-device-shop classes seeded");
    Ok(())
}

/// Mint the tenant's company-identity Subject via
/// `POST /api/subjects/company`. Idempotent (the endpoint upserts).
fn mint_company_identity(
    client: &Client,
    api_base: &str,
    tenant_id: &str,
    display_name: &str,
) -> Result<()> {
    let url = format!("{}/api/subjects/company", api_base.trim_end_matches('/'));
    let resp = client
        .post(&url)
        .header("x-sim-origin", "true")
        .json(&json!({ "id": tenant_id, "label": display_name }))
        .send()
        .with_context(|| format!("POST {url}"))?;
    let status = resp.status();
    if !status.is_success() {
        anyhow::bail!("POST {url} → {status} {}", resp.text().unwrap_or_default());
    }
    Ok(())
}

/// Split a roster into (rows-with-manager_id-stripped, manager
/// assignments). Two-pass seeding satisfies the manager_id self-FK
/// without topologically sorting the roster: every employee lands
/// first, then each manager edge is PUT back.
fn manager_split(
    mut roster: Vec<serde_json::Value>,
) -> (Vec<serde_json::Value>, Vec<(String, String)>) {
    let mut assignments = Vec::new();
    for emp in &mut roster {
        if let Some(obj) = emp.as_object_mut() {
            let id = obj
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if let Some(mgr_val) = obj.remove("manager_id")
                && let Some(mgr) = mgr_val.as_str()
                && !id.is_empty()
                && !mgr.is_empty()
            {
                assignments.push((id, mgr.to_string()));
            }
        }
    }
    (roster, assignments)
}

/// POST every row in `data/employees.json` to /api/people, then PUT
/// the manager edges back — the same two-pass idiom the brewery's
/// seed_employees uses. Idempotent: 409 Conflict on a duplicate id
/// is success. Hard-fails if any create is refused — the Workflows
/// published afterwards assign work to this roster.
///
/// Returns the roster size so the people-projection barrier knows
/// what count to wait for.
fn seed_employees(
    client: &Client,
    people_base: &str,
    headers: &reqwest::header::HeaderMap,
    seeds_dir: &Path,
) -> Result<usize> {
    let path = data_dir(seeds_dir).join("employees.json");
    let bytes = std::fs::read(&path).with_context(|| format!("read {}", path.display()))?;
    let roster: Vec<serde_json::Value> =
        serde_json::from_slice(&bytes).with_context(|| format!("parsing {}", path.display()))?;
    let roster_len = roster.len();
    let (roster, manager_assignments) = manager_split(roster);

    let url = format!("{}/api/people", people_base.trim_end_matches('/'));
    let mut posted = 0usize;
    let mut skipped = 0usize;
    let mut failed = 0usize;
    for emp in &roster {
        let resp = match client.post(&url).headers(headers.clone()).json(emp).send() {
            Ok(r) => r,
            Err(e) => {
                warn!(error = %e, "POST employee transport error");
                failed += 1;
                continue;
            }
        };
        let status = resp.status();
        if status.is_success() {
            posted += 1;
        } else if status.as_u16() == 409 {
            skipped += 1;
        } else {
            let id = emp.get("id").and_then(|v| v.as_str()).unwrap_or("?");
            let body = resp.text().unwrap_or_default();
            warn!(%id, %status, body = %body, "POST employee failed");
            failed += 1;
        }
    }
    if failed > 0 {
        anyhow::bail!(
            "{failed} employee POSTs failed (posted={posted}, skipped={skipped}). \
             Refusing to continue — the Workflows published next assign work to this roster."
        );
    }

    // Pass 2: link managers. Best-effort per edge — a failed link
    // degrades the org chart, not the install.
    let mut linked = 0usize;
    for (emp_id, mgr_id) in &manager_assignments {
        let row_url = format!(
            "{}/api/people/{}",
            people_base.trim_end_matches('/'),
            emp_id
        );
        let current: serde_json::Value = match client.get(&row_url).headers(headers.clone()).send()
        {
            Ok(r) if r.status().is_success() => match r.json() {
                Ok(v) => v,
                Err(e) => {
                    warn!(%emp_id, error = %e, "GET employee body decode failed");
                    continue;
                }
            },
            Ok(r) => {
                warn!(%emp_id, status = %r.status(), "GET employee for manager link failed");
                continue;
            }
            Err(e) => {
                warn!(%emp_id, error = %e, "GET employee transport error");
                continue;
            }
        };
        let mut body = current;
        if let Some(obj) = body.as_object_mut() {
            obj.insert(
                "manager_id".into(),
                serde_json::Value::String(mgr_id.clone()),
            );
        }
        match client
            .put(&row_url)
            .headers(headers.clone())
            .json(&body)
            .send()
        {
            Ok(r) if r.status().is_success() => linked += 1,
            Ok(r) => warn!(%emp_id, status = %r.status(), "PUT manager link failed"),
            Err(e) => warn!(%emp_id, error = %e, "PUT manager link transport error"),
        }
    }
    info!(
        posted,
        skipped,
        linked,
        total = roster_len,
        "used-device-shop roster seeded via /api/people"
    );
    Ok(roster_len)
}

/// POST each model in `data/catalog.json` to /api/catalog/models —
/// the same door the brewery's equipment-catalog seed uses. 409 =
/// already seeded, success.
fn seed_catalog_models(
    client: &Client,
    api_base: &str,
    headers: &reqwest::header::HeaderMap,
    seeds_dir: &Path,
) -> Result<()> {
    let path = data_dir(seeds_dir).join("catalog.json");
    let body =
        std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    let models: Vec<serde_json::Value> =
        serde_json::from_str(&body).with_context(|| format!("parse {}", path.display()))?;
    let count = models.len();
    let url = format!("{}/api/catalog/models", api_base.trim_end_matches('/'));
    for model in models {
        let resp = client
            .post(&url)
            .headers(headers.clone())
            .json(&model)
            .send()
            .with_context(|| format!("POST {url}"))?;
        let status = resp.status();
        if !status.is_success() && status.as_u16() != 409 {
            let body = resp.text().unwrap_or_default();
            anyhow::bail!("POST {url} returned {status}: {body}");
        }
    }
    info!(count, "used-device-shop device catalog seeded");
    Ok(())
}

/// Block until the people read-model reflects the just-seeded
/// roster, so the role-bearing steps opened immediately after (the
/// `workflow-design` Jobs) can be assigned to real holders instead
/// of dead-lettering against a cold roster. Same barrier the
/// brewery's prepare runs, thresholded on this tenant's roster size
/// instead of the brewery's (76 people here vs 400+ there).
/// Best-effort: a timeout logs and proceeds rather than aborting.
fn wait_for_people_projection(client: &Client, people_base: &str, roster_len: usize) {
    let url = format!("{}/api/people", people_base.trim_end_matches('/'));
    // The projection also holds rows the roster doesn't own (the
    // bootstrap-admin at minimum), so >= roster_len is reachable.
    let threshold = roster_len;
    let (mut prev, mut stable) = (0usize, 0u32);
    for _ in 0..90 {
        let count = client
            .get(&url)
            .header("x-boss-user", SEED_USER)
            .send()
            .ok()
            .and_then(|r| r.json::<serde_json::Value>().ok())
            .and_then(|v| v.as_array().map(|a| a.len()))
            .unwrap_or(0);
        if count >= threshold && count == prev {
            stable += 1;
            if stable >= 3 {
                info!(people = count, "roster ready — opening design Jobs");
                return;
            }
        } else {
            stable = 0;
        }
        prev = count;
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
    info!(
        people = prev,
        threshold, "people projection did not stabilize in 90s; opening design Jobs anyway"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seeds_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("..")
            .join("examples/used-device-shop/seeds")
    }

    /// The prepare step's inputs, pinned: a renamed or dropped seed
    /// file should fail here, in a unit test that names it, not on a
    /// fresh VM an hour into a bootstrap.
    #[test]
    fn seed_bundle_is_complete_for_prepare() {
        let seeds = seeds_dir();
        for rel in [
            "classes.toml",
            "policy_rules.toml",
            "tenant.toml",
            "workflows.toml",
        ] {
            assert!(seeds.join(rel).exists(), "missing seed file: {rel}");
        }
        for rel in ["employees.json", "catalog.json"] {
            assert!(
                data_dir(&seeds).join(rel).exists(),
                "missing data file: {rel}"
            );
        }
    }

    #[test]
    fn classes_toml_parses_into_batch_rows() {
        let rows = load_class_rows(&seeds_dir().join("classes.toml")).unwrap();
        // 22 measured 2026-09-02: 8 asset categories + 8 document
        // kinds + 5 account-team roles + the auditor role. A floor,
        // not an equality — adding taxonomy rows shouldn't fail here.
        assert!(
            rows.len() >= 22,
            "expected the full tenant taxonomy; got {} rows",
            rows.len()
        );
        for row in &rows {
            for key in ["subject_kind", "code", "display_name"] {
                assert!(
                    row.get(key).and_then(|v| v.as_str()).is_some(),
                    "class row missing `{key}`: {row}"
                );
            }
        }
    }

    /// `auditor` is the one employee role the platform registry
    /// (01-registries.sql) does not ship and this tenant's roster
    /// uses (emp-201). boss-people-api refuses employee writes whose
    /// role has no active Class, so the tenant seed must carry it —
    /// this test names the row if it drops out.
    #[test]
    fn classes_toml_carries_the_auditor_role() {
        let rows = load_class_rows(&seeds_dir().join("classes.toml")).unwrap();
        assert!(
            rows.iter().any(|r| {
                r.get("subject_kind").and_then(|v| v.as_str()) == Some("employee")
                    && r.get("member_attribute").and_then(|v| v.as_str()) == Some("role")
                    && r.get("code").and_then(|v| v.as_str()) == Some("auditor")
            }),
            "classes.toml must seed the employee `auditor` role class"
        );
    }

    /// A fact that lives twice gets an equality test (CLAUDE.md §9a):
    /// catalog models declare a `category` the catalog-api validates
    /// against (subject_kind='asset', member_attribute='category')
    /// Class rows. The categories live in data/catalog.json, the
    /// Class rows in seeds/classes.toml — if they drift, the catalog
    /// seed is refused at install time. This names the offender
    /// instead.
    #[test]
    fn every_catalog_category_has_a_class_row() {
        let models: Vec<serde_json::Value> = serde_json::from_str(
            &std::fs::read_to_string(data_dir(&seeds_dir()).join("catalog.json")).unwrap(),
        )
        .unwrap();
        let seeded: std::collections::HashSet<String> =
            load_class_rows(&seeds_dir().join("classes.toml"))
                .unwrap()
                .iter()
                .filter(|r| {
                    r.get("subject_kind").and_then(|v| v.as_str()) == Some("asset")
                        && r.get("member_attribute").and_then(|v| v.as_str()) == Some("category")
                })
                .filter_map(|r| r.get("code").and_then(|v| v.as_str()))
                .map(str::to_string)
                .collect();
        for model in &models {
            let category = model
                .get("category")
                .and_then(|v| v.as_str())
                .expect("catalog model without a category");
            assert!(
                seeded.contains(category),
                "catalog.json category `{category}` has no asset-category Class row in classes.toml"
            );
        }
    }

    #[test]
    fn manager_split_strips_and_collects_edges() {
        let roster: Vec<serde_json::Value> = serde_json::from_slice(
            &std::fs::read(data_dir(&seeds_dir()).join("employees.json")).unwrap(),
        )
        .unwrap();
        let total = roster.len();
        let ids: std::collections::HashSet<String> = roster
            .iter()
            .filter_map(|e| e.get("id").and_then(|v| v.as_str()))
            .map(str::to_string)
            .collect();

        let (stripped, edges) = manager_split(roster);
        assert_eq!(stripped.len(), total, "no rows lost");
        assert!(
            stripped.iter().all(|e| e.get("manager_id").is_none()),
            "pass-1 rows must not carry manager_id"
        );
        assert!(!edges.is_empty(), "the roster has a reporting graph");
        for (emp, mgr) in &edges {
            assert!(ids.contains(emp), "edge from unknown employee {emp}");
            assert!(
                ids.contains(mgr),
                "employee {emp} reports to unknown manager {mgr}"
            );
        }
    }
}
