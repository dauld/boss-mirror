//! `boss tenant publish <dir> [--gateway <url>] [--dry-run]` — a tenant
//! with no engine is published through the shared doors (backlog
//! ee7b62bb).
//!
//! WHY. Measured 2026-09-16: a tenant was PUBLISHED into a running
//! deployment only by a tenant engine's prepare — the sim tenant's
//! (`<engine> prepare`, driven by its infra/seed-*-tenant.sh) and the
//! device shop's (crates/tenants/*). Both compose the same
//! shared doors (POST /api/classes/batch, the company Subject,
//! `boss_policy::bootstrap::publish_policy_rules`, POST /api/people in
//! two passes, `boss_jobs::bootstrap::publish_workflows`) around
//! engine-only data (accounts, vendors, messages, catalog, opening
//! balances). The real company's tenant (David's own tenant repo, the
//! first real customer of fcc1d57b) has no engine and never will — it
//! is a real company, load comes from the world — so nothing could
//! publish it. This verb is the engines' shared composition with the
//! engine-only data left out, which is what makes the cutover
//! (ffc83387) a tenant-source flip and not a rewrite of the sim
//! tenant's engine. (The tenants are not named here: the vocabulary
//! ratchet, be39298f, counts a tenant's name above its tier.)
//!
//! THE PLAN IS THE CONTRACT, IN DEPENDENCY ORDER. [`plan`] walks the
//! directory the way the engines' prepare does: classes (employee and
//! account writes validate against them) → the chart of accounts (the
//! ledger's rows, after the classes; backlog 41af5195) → locations (an
//! employee's `location` FKs into them; backlog 1ec8312a) → business
//! calendars →
//! the company Subject → policy grants → people (two passes: create,
//! then link managers) → agents (the machine half of the roster; a
//! step's audience may name one; backlog f56155f0) → Workflows, after
//! a barrier on the people projection (a publish opens design Jobs
//! whose role-bearing steps are assigned against the roster) →
//! sensors → the ledger's posting rules → its event→fact projections
//! (a projection names the fact kind a rule posts and the workflow
//! whose step it reads; backlog a40541cb) → dispatcher rules LAST (the
//! tenant's own reactors, backlog 458971ef; a rule fires on the
//! protocols published before it, and its args may name the projection
//! rules too). Every file present in the directory gets a line: a door
//! and a count when publish writes it, `skipped: <why>` when nothing
//! reads it — never silence. `--dry-run` prints that plan and makes no
//! HTTP call.
//!
//! IT REFUSES BEFORE IT WRITES. The plan is built on [`tenant::check`]
//! and a directory with a MISSING or INVALID file is refused whole,
//! with the plan still printed so the operator sees every write beside
//! the file that blocks them. The doors read each file with the same
//! loader check does, so an INVALID file would fail the publish midway
//! and leave a half-published tenant. Under the launcher
//! (infra/seed-tenant.sh) that refusal is a DEGRADED pod that retries —
//! the contract tenant-launch.sh already holds.
//!
//! IDEMPOTENT, LIKE THE ENGINES. The classes batch inserts if absent;
//! the calendar batch replaces by code; the company Subject upserts;
//! policy GETs each rule before it POSTs; a 409 on an employee is
//! "already there"; the workflow publish skips a kind an authoring Job
//! already published; a dispatcher rule at its file's version is
//! `present`. A second run writes nothing new.
//!
//! SIGNED, NOT SIMULATED. Every write carries `x-boss-user` as
//! `automation:tenant-seed` (platform-admin / operator — the tier the
//! doors gate on) and NOT `x-sim-origin`: the engines mark their seed
//! writes as a sim chain, which stamps the resulting events
//! `_simulated`, and the cutover TRIMS simulated rows. A real company's
//! declarations are real.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use boss_core::tenant_manifest::TenantToml;
use reqwest::blocking::Client;
use serde_json::{Value, json};
use tracing::{info, warn};

use crate::tenant::{self, Status};

/// The x-boss-user identity every publish write carries. A dedicated
/// seed identity, not a person: "the tenant directory landed these
/// rows" is the provenance the audit log should read, the same
/// reasoning as the engines' `automation:<tenant>-seed` identities.
pub const SEED_USER: &str = r#"{"id":"automation:tenant-seed","role":"platform-admin","access_tier":"operator","territory_account_ids":[],"direct_report_ids":[],"department":"platform"}"#;

/// Where each door lives. `--gateway` routes every `/api` prefix
/// through one base; the default is each service's own localhost port
/// from `boss_ports` — the in-pod launcher path, where the gateway
/// starts AFTER the tenant is published.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bases {
    pub classes: String,
    pub ledger: String,
    pub locations: String,
    pub calendar: String,
    pub subjects: String,
    pub policy: String,
    pub people: String,
    pub jobs: String,
    /// The rule-authoring door (backlog 458971ef); the gateway proxies
    /// `/api/dispatcher/*` to it, the launcher path is its own port.
    pub dispatcher: String,
}

impl Bases {
    pub fn resolve(gateway: Option<&str>) -> Self {
        let resolve = |service: &str| {
            gateway
                .map(|g| g.trim_end_matches('/').to_string())
                .unwrap_or_else(|| boss_ports::url(service))
        };
        Self {
            classes: resolve("classes"),
            ledger: resolve("ledger"),
            locations: resolve("locations"),
            calendar: resolve("calendar"),
            subjects: resolve("subject-kinds"),
            policy: resolve("policy"),
            people: resolve("people"),
            jobs: resolve("jobs"),
            dispatcher: resolve("dispatcher"),
        }
    }

    /// One line for the report header: what a write was routed to.
    pub fn describe(&self, gateway: Option<&str>) -> String {
        match gateway {
            Some(g) => format!("routing: every /api prefix through gateway {g}"),
            None => "routing: each service's own localhost port (boss_ports)".to_string(),
        }
    }
}

/// One shared door and what this directory sends through it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Door {
    Classes {
        rows: Vec<Value>,
    },
    /// The tenant's chart of accounts (backlog 41af5195, 2026-09-17),
    /// as the ledger door's own rows. After the classes; a code the
    /// starter chart holds is kept and the line names the difference.
    Chart {
        rows: Vec<boss_ledger::chart::AccountInput>,
    },
    /// The tenant's sites (backlog 1ec8312a, 2026-09-17), as the
    /// batch endpoint's JSON rows. Before the roster: an employee's
    /// `location` is a foreign key into the registry, and until this
    /// door the only rows were the schema's.
    Locations {
        rows: Vec<Value>,
    },
    Calendars {
        body: String,
        count: usize,
    },
    Company {
        id: String,
        label: String,
    },
    Policy {
        path: PathBuf,
        rules: usize,
    },
    People {
        roster: Vec<Value>,
    },
    /// The tenant's registered agents (backlog f56155f0, 2026-09-17):
    /// the machine half of the roster. After the people, before the
    /// Workflows — a step's audience may name an agent by id.
    Agents {
        rows: Vec<boss_jobs::agents::AgentInput>,
    },
    Workflows {
        path: PathBuf,
        owning_team: String,
        kinds: Vec<String>,
    },
    /// The tenant's sensor declarations (design 14c9b2ad): what the
    /// platform polls and what kind each reading opens. After the
    /// Workflows, because a row names one by kind.
    Sensors {
        tenant_id: String,
        rows: Vec<boss_jobs::sensors::SensorInput>,
    },
    /// The tenant's posting rules (backlog a40541cb): fact kind →
    /// journal lines, landed with `source = tenant:<id>`.
    PostingRules {
        tenant_id: String,
        rows: Vec<boss_ledger::posting_rules::PostingRuleInput>,
    },
    /// The tenant's event→fact projections, after the posting rules
    /// (a projection names the fact kind a rule posts).
    ProjectionRules {
        rows: Vec<boss_ledger::posting_rules::ProjectionRule>,
    },
    /// The tenant's dispatcher rules (backlog 458971ef): its own
    /// reactors, in the product rule file's shape, published through
    /// the dispatcher's authoring door one rule at a time, each row
    /// carrying `source = tenant:<tenant_id>`. Last of all — a reactor
    /// fires on the protocols published before it.
    Rules {
        tenant_id: String,
        rules: Vec<boss_dispatcher::rules::registry::RawRule>,
    },
}

impl Door {
    /// The door, in the wire's own words.
    pub fn label(&self) -> &'static str {
        match self {
            Door::Classes { .. } => "POST /api/classes/batch",
            Door::Chart { .. } => "POST /api/ledger/accounts/batch",
            Door::Locations { .. } => "POST /api/locations/batch",
            Door::Calendars { .. } => "POST /api/calendar/business-calendars/batch",
            Door::Company { .. } => "POST /api/subjects/company",
            Door::Policy { .. } => "GET+POST /api/policy/rules",
            Door::People { .. } => "POST /api/people, then PUT manager links",
            Door::Agents { .. } => "POST /api/agents/batch",
            Door::Workflows { .. } => "POST /api/jobs (workflow-design), walked to publish",
            Door::Sensors { .. } => "POST /api/sensors/batch",
            Door::PostingRules { .. } => "POST /api/ledger/posting-rules/batch",
            Door::ProjectionRules { .. } => "POST /api/ledger/fact-projection-rules/batch",
            Door::Rules { .. } => {
                "POST /api/dispatcher/rules/_validate, GET versions, then draft + publish per rule"
            }
        }
    }

    /// What goes through it, and why a second run writes nothing new.
    pub fn what(&self) -> String {
        match self {
            Door::Classes { rows } => format!("{} classes (insert-if-absent)", rows.len()),
            Door::Chart { rows } => format!(
                "{} accounts (insert-if-absent by code: {}; a code the starter chart holds is kept and a differing field is named)",
                rows.len(),
                rows.iter()
                    .map(|a| a.code.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Door::Locations { rows } => format!(
                "{} locations (insert-if-absent by id: {})",
                rows.len(),
                rows.iter()
                    .filter_map(|r| r.get("id").and_then(Value::as_str))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Door::Calendars { count, .. } => format!("{count} calendars (replaced by code)"),
            Door::Company { id, label } => format!("company `{id}` ({label}) (upsert)"),
            Door::Policy { rules, .. } => {
                format!("{rules} rules (each GET first; an existing rule is kept)")
            }
            Door::People { roster } => format!(
                "{} people, {} manager links (409 = already there)",
                roster.len(),
                manager_split(roster.clone()).1.len()
            ),
            Door::Agents { rows } => format!(
                "{} agents (insert-if-absent by id: {}; a registered row is kept and a differing field is named)",
                rows.len(),
                rows.iter()
                    .map(|a| a.id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Door::Workflows {
                owning_team, kinds, ..
            } => format!(
                "{} workflows as owning_team `{owning_team}` ({}) (a kind an authoring Job already published is skipped)",
                kinds.len(),
                kinds.join(", ")
            ),
            Door::Sensors { rows, .. } => format!(
                "{} sensors (insert-if-absent by id: {})",
                rows.len(),
                rows.iter()
                    .map(|r| r.id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Door::PostingRules { rows, .. } => format!(
                "{} posting rules (insert-if-absent by fact_kind + version: {}; a differing row under a kept version is named)",
                rows.len(),
                rows.iter()
                    .map(|r| format!("{} v{}", r.fact_kind, r.version))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Door::ProjectionRules { rows } => format!(
                "{} projections (insert-if-absent by event_kind + when: {})",
                rows.len(),
                rows.iter()
                    .map(|r| r.event_kind.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Door::Rules { tenant_id, rules } => format!(
                "{} rules as source tenant:{tenant_id} ({}) (append-only: an unchanged version is a no-op, a higher one supersedes)",
                rules.len(),
                rules
                    .iter()
                    .map(|r| format!("{} v{}", r.name, r.version))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Write(Door),
    /// Nothing reads this file; the why is printed, never implied.
    Skip(String),
    /// Check refused it: the loader's own words. One of these refuses
    /// the whole publish.
    Refused(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    /// Relative to the tenant directory, as found.
    pub path: String,
    pub action: Action,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub dir: PathBuf,
    pub tenant_id: String,
    pub steps: Vec<Step>,
}

impl Plan {
    pub fn refusals(&self) -> Vec<&Step> {
        self.steps
            .iter()
            .filter(|s| matches!(s.action, Action::Refused(_)))
            .collect()
    }

    pub fn publishable(&self) -> bool {
        self.refusals().is_empty()
    }

    fn write_count(&self) -> usize {
        self.steps
            .iter()
            .filter(|s| matches!(s.action, Action::Write(_)))
            .count()
    }

    fn width(&self) -> usize {
        self.steps.iter().map(|s| s.path.len()).max().unwrap_or(0)
    }

    /// One line per step; `outcome` is what a real run appended.
    pub fn render_step(&self, step: &Step, outcome: Option<&str>) -> String {
        let width = self.width();
        let body = match &step.action {
            Action::Write(door) => format!("{}  {}", door.label(), door.what()),
            Action::Skip(why) => format!("skipped: {why}"),
            Action::Refused(why) => format!("REFUSED: {}", why.trim_end().replace('\n', " | ")),
        };
        match outcome {
            Some(o) => format!("  {:<width$}  {body}  → {o}", step.path, width = width),
            None => format!("  {:<width$}  {body}", step.path, width = width),
        }
    }

    pub fn render_header(&self, dry_run: bool) -> String {
        format!(
            "boss tenant publish {} (tenant_id {}){}",
            self.dir.display(),
            self.tenant_id,
            if dry_run { " — dry-run, no HTTP" } else { "" }
        )
    }

    pub fn render_footer(&self) -> String {
        let refused = self.refusals().len();
        if refused > 0 {
            format!(
                "{} writes planned, {refused} file(s) REFUSED — nothing is published until they pass `boss tenant check`",
                self.write_count()
            )
        } else {
            format!(
                "{} writes, signing as automation:tenant-seed",
                self.write_count()
            )
        }
    }
}

fn read(path: &Path) -> Result<String> {
    std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))
}

/// The first of `paths` present under `dir`, relative.
fn present(dir: &Path, paths: &[&str]) -> Option<String> {
    paths
        .iter()
        .find(|p| dir.join(p).is_file())
        .map(|p| (*p).to_string())
}

/// `seeds/classes.json` (a JSON array) or `seeds/classes.toml`
/// (`[[class]]` rows) as the JSON rows the batch endpoint takes — the
/// same two spellings the two engines author, both already validated
/// as `ClassInput` by check.
fn load_class_rows(path: &Path) -> Result<Vec<Value>> {
    let text = read(path)?;
    if path.extension().and_then(|e| e.to_str()) == Some("toml") {
        #[derive(serde::Deserialize)]
        struct Bundle {
            #[serde(default)]
            class: Vec<Value>,
        }
        let b: Bundle =
            toml::from_str(&text).with_context(|| format!("parse {}", path.display()))?;
        Ok(b.class)
    } else {
        serde_json::from_str(&text).with_context(|| format!("parse {}", path.display()))
    }
}

/// `seeds/locations.toml` `[[location]]` rows as the JSON rows the
/// batch endpoint takes (a bare array, the classes shape) — already
/// validated as `boss_locations::http::LocationInput` by check.
fn load_location_rows(path: &Path) -> Result<Vec<Value>> {
    #[derive(serde::Deserialize)]
    struct Bundle {
        #[serde(default)]
        location: Vec<Value>,
    }
    let b: Bundle =
        toml::from_str(&read(path)?).with_context(|| format!("parse {}", path.display()))?;
    Ok(b.location)
}

/// `seeds/operator_hires.toml` `[[hire]]` rows — the Employee shape at
/// a second spelling, through the same door.
fn load_hire_rows(path: &Path) -> Result<Vec<Value>> {
    #[derive(serde::Deserialize)]
    struct Bundle {
        #[serde(default)]
        hire: Vec<Value>,
    }
    let b: Bundle =
        toml::from_str(&read(path)?).with_context(|| format!("parse {}", path.display()))?;
    Ok(b.hire)
}

/// Split a roster into (rows with manager_id stripped, manager
/// assignments). Two passes satisfy the manager_id self-FK without
/// sorting the roster — the idiom both engines' seed_employees use.
fn manager_split(mut roster: Vec<Value>) -> (Vec<Value>, Vec<(String, String)>) {
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

/// Why a file the contract names but publish does not send is skipped:
/// the contract's own `read_by`, so the plan cannot say "no reader" of
/// a file an engine reads, nor imply a door for one nothing reads.
fn skip_reason(path: &str, status: Status) -> String {
    if status == Status::Unknown {
        return "no reader — not named by the contract (docs/tenant-contract.md)".to_string();
    }
    let read_by = tenant::CONTRACT
        .iter()
        .find(|e| e.paths.contains(&path))
        .map(|e| e.read_by)
        .unwrap_or("");
    if read_by.starts_with("NO READER") {
        "no reader (measured 2026-09-16)".to_string()
    } else {
        let who = read_by
            .split(['(', ','])
            .next()
            .unwrap_or(read_by)
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        format!("tenant-engine data, no shared door — read by {who}")
    }
}

/// Build the plan: pure over the filesystem, never touches the
/// network. Every file check reports gets exactly one step.
pub fn plan(dir: &Path) -> Result<Plan> {
    let report = tenant::check(dir);
    let status_of = |rel: &str| -> Option<Status> {
        report.rows.iter().find(|r| r.path == rel).map(|r| r.status)
    };
    let detail_of = |rel: &str| -> String {
        report
            .rows
            .iter()
            .find(|r| r.path == rel)
            .map(|r| r.detail.clone())
            .unwrap_or_default()
    };

    // The manifest: tenant_id is every workflow's owning_team and the
    // company Subject's id; check already refused a manifest without
    // one, so a missing id here is the refusal row, not a second
    // error.
    let manifest_rel = present(dir, &["tenant.toml", "seeds/tenant.toml"]);
    let manifest = manifest_rel
        .as_deref()
        .filter(|rel| status_of(rel) == Some(Status::Ok))
        .and_then(|rel| read(&dir.join(rel)).ok())
        .and_then(|t| TenantToml::parse(&t).ok());
    let tenant_id = manifest
        .as_ref()
        .and_then(|m| m.meta.tenant_id.clone())
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "tenant".to_string());
    let display_name = manifest
        .as_ref()
        .and_then(|m| m.meta.display_name.clone())
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| tenant_id.clone());

    let mut steps: Vec<Step> = Vec::new();
    let mut consumed: BTreeSet<String> = BTreeSet::new();
    // One step per door, in dependency order. A file check refused
    // becomes a Refused step at the door's position; an absent
    // optional file is no step at all.
    let mut door = |rel: Option<String>, load: &dyn Fn(&Path) -> Result<Door>| -> Result<()> {
        let Some(rel) = rel else { return Ok(()) };
        consumed.insert(rel.clone());
        let action = match status_of(&rel) {
            Some(Status::Ok) => Action::Write(load(&dir.join(&rel))?),
            Some(Status::Missing) => Action::Refused("required, missing".to_string()),
            _ => Action::Refused(detail_of(&rel)),
        };
        steps.push(Step { path: rel, action });
        Ok(())
    };

    // 1. Classes — employee role/department and account-type writes
    //    validate against them.
    door(
        present(dir, &["seeds/classes.json", "seeds/classes.toml"]),
        &|p| {
            Ok(Door::Classes {
                rows: load_class_rows(p)?,
            })
        },
    )?;
    // 1b. The chart of accounts (backlog 41af5195) — the ledger's
    //    rows, after the classes (the design's order; nothing here
    //    FKs into them yet) and before anything that could post.
    //    Insert-if-absent by code: a code the starter chart holds is
    //    kept under the starter's name and the line names the field.
    door(present(dir, &["seeds/chart_of_accounts.toml"]), &|p| {
        Ok(Door::Chart {
            rows: boss_ledger::chart::load_chart_toml(p).map_err(anyhow::Error::msg)?,
        })
    })?;
    // 2. Locations (backlog 1ec8312a) — a row's `kind` is a Class
    //    code, and `employees.location` is a foreign key into the
    //    registry, so the sites go after the classes and before the
    //    roster. Until 2026-09-17 nothing read this file and the
    //    people door refused a founder at the tenant's own HQ.
    door(present(dir, &["seeds/locations.toml"]), &|p| {
        Ok(Door::Locations {
            rows: load_location_rows(p)?,
        })
    })?;
    // 3. Business calendars — reference data the dispatcher's timing
    //    triggers resolve business days from; before anything that
    //    consumes them.
    door(present(dir, &["seeds/business_calendars.json"]), &|p| {
        let body = read(p)?;
        let rows: Vec<Value> = serde_json::from_str(&body)?;
        Ok(Door::Calendars {
            body,
            count: rows.len(),
        })
    })?;
    // 4. The company Subject — the organization being modeled is
    //    itself a Subject; org-level Workflows open Jobs about it.
    //    A missing required manifest is the one Refused step that
    //    has no file to name, so it is named here.
    door(
        Some(manifest_rel.unwrap_or_else(|| "tenant.toml".to_string())),
        &|_| {
            Ok(Door::Company {
                id: tenant_id.clone(),
                label: display_name.clone(),
            })
        },
    )?;
    // 5. Policy grants — capability-level, so before the roster and
    //    the Workflows whose design Jobs need `workflow-approver`.
    door(present(dir, &["seeds/policy_rules.toml"]), &|p| {
        let rules = boss_policy_client::seed_loader::load_policy_rules(p)?;
        Ok(Door::Policy {
            path: p.to_path_buf(),
            rules: rules.len(),
        })
    })?;
    // 6. People — before Workflows, so the dispatcher's role-bearing
    //    auto-assignment resolves against a real roster.
    door(present(dir, &["seeds/employees.json"]), &|p| {
        Ok(Door::People {
            roster: serde_json::from_str(&read(p)?)?,
        })
    })?;
    door(present(dir, &["seeds/operator_hires.toml"]), &|p| {
        Ok(Door::People {
            roster: load_hire_rows(p)?,
        })
    })?;
    // 7. Agents (backlog f56155f0) — the machine half of the roster,
    //    before the Workflows: a step's audience may name an agent by
    //    id, and the login door resolves a declared alias from here on.
    door(present(dir, &["seeds/agents.toml"]), &|p| {
        Ok(Door::Agents {
            rows: boss_jobs::agents::load_agents_toml(p).map_err(anyhow::Error::msg)?,
        })
    })?;
    // 8. Workflows — after everything a packet needs.
    door(present(dir, &["seeds/workflows.toml"]), &|p| {
        let specs = boss_jobs::seed_loader::load_workflows_with_owning_team(p, &tenant_id)?;
        Ok(Door::Workflows {
            path: p.to_path_buf(),
            owning_team: tenant_id.clone(),
            kinds: specs.iter().map(|s| s.kind.clone()).collect(),
        })
    })?;
    // 9. Sensors (design 14c9b2ad): each row names the workflow kind
    //    a reading opens, so the kinds go first; the registry row is
    //    what the platform's 5-minute poll reads.
    door(present(dir, &["seeds/sensors.toml"]), &|p| {
        Ok(Door::Sensors {
            tenant_id: tenant_id.clone(),
            rows: boss_jobs::sensors::load_sensors_toml(p).map_err(anyhow::Error::msg)?,
        })
    })?;
    // 10. Posting rules (backlog a40541cb): fact kind → journal lines,
    //     landed with the tenant as their source.
    door(present(dir, &["seeds/posting_rules.toml"]), &|p| {
        Ok(Door::PostingRules {
            tenant_id: tenant_id.clone(),
            rows: boss_ledger::posting_rules::load_posting_rules_toml(p)
                .map_err(anyhow::Error::msg)?,
        })
    })?;
    // 11. Event→fact projections: a projection names the fact kind a
    //     posting rule posts and (through `when`) the workflow whose
    //     step it reads, so both go first.
    door(present(dir, &["seeds/fact_projection_rules.toml"]), &|p| {
        Ok(Door::ProjectionRules {
            rows: boss_ledger::posting_rules::load_projection_rules_toml(p)
                .map_err(anyhow::Error::msg)?,
        })
    })?;
    // 12. Rules LAST (backlog 458971ef): a reactor's `when` and args
    //     name the kinds and steps of protocols above (and the fact
    //     kinds the projections above turn them into), and a rule live
    //     before its protocol would fire on nothing or on the wrong
    //     packet. Read with the dispatcher's own parser, as check did.
    door(present(dir, &["seeds/rules.toml"]), &|p| {
        Ok(Door::Rules {
            tenant_id: tenant_id.clone(),
            rules: boss_dispatcher::rules::registry::parse_raw_file(p)?.rules,
        })
    })?;
    // Everything else check saw, in check's order: a required file it
    // found missing (no door claimed it — there was no file) is still
    // one line and still a refusal; the rest are skipped, with why.
    for r in report.rows.iter().filter(|r| !consumed.contains(&r.path)) {
        steps.push(Step {
            path: r.path.clone(),
            action: match r.status {
                Status::Missing => Action::Refused("required, missing".to_string()),
                Status::Invalid => Action::Refused(r.detail.clone()),
                s => Action::Skip(skip_reason(&r.path, s)),
            },
        });
    }
    Ok(Plan {
        dir: dir.to_path_buf(),
        tenant_id,
        steps,
    })
}

fn seed_client() -> Result<Client> {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        "x-boss-user",
        reqwest::header::HeaderValue::from_static(SEED_USER),
    );
    headers.insert(
        reqwest::header::CONTENT_TYPE,
        reqwest::header::HeaderValue::from_static("application/json"),
    );
    Ok(Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .default_headers(headers)
        .build()?)
}

fn url(base: &str, path: &str) -> String {
    format!("{}{path}", base.trim_end_matches('/'))
}

fn refuse(resp: reqwest::blocking::Response, what: &str) -> Result<reqwest::blocking::Response> {
    let status = resp.status();
    if status.is_success() {
        Ok(resp)
    } else {
        bail!("{what} → {status} {}", resp.text().unwrap_or_default())
    }
}

/// POST every roster row, then PUT the manager edges back. 409 on a
/// duplicate id is "already there"; any other refusal fails the
/// publish — the Workflows published next assign work to this roster.
///
/// A 409 IS "ALREADY THERE" ONLY WHEN THE ROW IS (backlog 0d2d7daa,
/// 2026-09-16). The people API answers 409 for every
/// `PeopleError::Conflict`: a duplicate id, but also a role with no
/// active Class or a `location` the registry does not hold. Counting
/// each as "already there" skipped the real company's founder in
/// silence — its `location` names a site no door seeds — and would
/// have published the Workflows against an empty roster. So a 409 is
/// followed by GET /api/people/{id}: 200 is already there; anything
/// else is the refusal it was, with the API's words.
fn seed_people(client: &Client, people_base: &str, roster: &[Value]) -> Result<String> {
    let (rows, links) = manager_split(roster.to_vec());
    let post_url = url(people_base, "/api/people");
    let (mut posted, mut already) = (0usize, 0usize);
    for emp in &rows {
        let resp = client
            .post(&post_url)
            .json(emp)
            .send()
            .with_context(|| format!("POST {post_url}"))?;
        let id = emp.get("id").and_then(Value::as_str).unwrap_or("?");
        match resp.status().as_u16() {
            409 => {
                let row_url = url(people_base, &format!("/api/people/{id}"));
                let exists = client
                    .get(&row_url)
                    .send()
                    .with_context(|| format!("GET {row_url} after a 409"))?
                    .status()
                    .is_success();
                if exists {
                    already += 1;
                } else {
                    bail!(
                        "POST {post_url} ({id}) → 409 {} — and GET {row_url} says the row is \
                         not there, so this is a refusal, not a duplicate",
                        resp.text().unwrap_or_default()
                    );
                }
            }
            s if (200..300).contains(&s) => posted += 1,
            _ => {
                refuse(resp, &format!("POST {post_url} ({id})"))?;
            }
        }
    }
    // Manager links are best-effort per edge — a failed link degrades
    // the org chart, not the publish (the engines' rule).
    let mut linked = 0usize;
    for (emp_id, mgr_id) in &links {
        let row_url = url(people_base, &format!("/api/people/{emp_id}"));
        let current = client
            .get(&row_url)
            .send()
            .ok()
            .filter(|r| r.status().is_success())
            .and_then(|r| r.json::<Value>().ok());
        let Some(mut body) = current else {
            warn!(%emp_id, "GET employee for manager link failed; edge not linked");
            continue;
        };
        if let Some(obj) = body.as_object_mut() {
            obj.insert("manager_id".into(), Value::String(mgr_id.clone()));
        }
        match client.put(&row_url).json(&body).send() {
            Ok(r) if r.status().is_success() => linked += 1,
            Ok(r) => warn!(%emp_id, status = %r.status(), "PUT manager link failed"),
            Err(e) => warn!(%emp_id, error = %e, "PUT manager link transport error"),
        }
    }
    Ok(format!(
        "{posted} posted, {already} already there, {linked}/{} linked",
        links.len()
    ))
}

/// Block until the people read-model holds at least the roster just
/// seeded, stable across three reads, or 90 s elapse — the barrier
/// both engines run before opening design Jobs, so role-bearing steps
/// assign to real holders instead of dead-lettering against a cold
/// roster. Best-effort: a timeout logs and proceeds.
fn wait_for_people_projection(client: &Client, people_base: &str, threshold: usize) {
    let list_url = url(people_base, "/api/people");
    let (mut prev, mut stable) = (0usize, 0u32);
    for _ in 0..90 {
        let count = client
            .get(&list_url)
            .send()
            .ok()
            .and_then(|r| r.json::<Value>().ok())
            .and_then(|v| v.as_array().map(|a| a.len()))
            .unwrap_or(0);
        if count >= threshold && count == prev {
            stable += 1;
            if stable >= 3 {
                info!(people = count, "roster ready — publishing workflows");
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
        threshold, "people projection did not stabilize in 90s; publishing workflows anyway"
    );
}

/// Send one door. Returns the outcome line's tail.
fn send(client: &Client, bases: &Bases, door: &Door) -> Result<String> {
    match door {
        Door::Classes { rows } => {
            let u = url(&bases.classes, "/api/classes/batch");
            let resp = refuse(client.post(&u).json(rows).send()?, &format!("POST {u}"))?;
            let body: Value = resp.json().unwrap_or(Value::Null);
            Ok(format!(
                "received {}, inserted {}",
                body.get("received").and_then(Value::as_u64).unwrap_or(0),
                body.get("inserted").and_then(Value::as_u64).unwrap_or(0)
            ))
        }
        Door::Chart { rows } => {
            let u = url(&bases.ledger, "/api/ledger/accounts/batch");
            let resp = refuse(client.post(&u).json(rows).send()?, &format!("POST {u}"))?;
            // The door's own outcome type: the line names a kept
            // row's differing fields the way the ledger reported
            // them — `1000 (name differs)` — never a count that
            // hides that the tenant's Bank is the starter's Cash.
            let out: boss_ledger::chart::ChartBatchOutcome = resp
                .json()
                .with_context(|| format!("POST {u}: the outcome did not parse"))?;
            Ok(out.summary())
        }
        Door::Locations { rows } => {
            let u = url(&bases.locations, "/api/locations/batch");
            let resp = refuse(client.post(&u).json(rows).send()?, &format!("POST {u}"))?;
            let body: Value = resp.json().unwrap_or(Value::Null);
            Ok(format!(
                "received {}, inserted {}",
                body.get("received").and_then(Value::as_u64).unwrap_or(0),
                body.get("inserted").and_then(Value::as_u64).unwrap_or(0)
            ))
        }
        Door::Calendars { body, count } => {
            let u = url(&bases.calendar, "/api/calendar/business-calendars/batch");
            refuse(
                client.post(&u).body(body.clone()).send()?,
                &format!("POST {u}"),
            )?;
            Ok(format!("{count} calendars landed"))
        }
        Door::Company { id, label } => {
            let u = url(&bases.subjects, "/api/subjects/company");
            refuse(
                client
                    .post(&u)
                    .json(&json!({ "id": id, "label": label }))
                    .send()?,
                &format!("POST {u}"),
            )?;
            Ok("upserted".to_string())
        }
        Door::Policy { path, .. } => {
            boss_policy::bootstrap::publish_policy_rules(
                &bases.policy,
                path,
                false,
                "tenant-seed",
                Some(SEED_USER),
            )?;
            Ok("ok (posted/skipped counts in the log)".to_string())
        }
        Door::People { roster } => seed_people(client, &bases.people, roster),
        Door::Agents { rows } => {
            let u = url(&bases.jobs, "/api/agents/batch");
            let resp = refuse(client.post(&u).json(rows).send()?, &format!("POST {u}"))?;
            // The door's own outcome type, so the line names a kept
            // row's differing fields the way the registry reported
            // them — never a count that hides a disagreement.
            let out: boss_jobs::agents::AgentsBatchOutcome = resp
                .json()
                .with_context(|| format!("POST {u}: the outcome did not parse"))?;
            Ok(out.summary())
        }
        Door::Workflows {
            path, owning_team, ..
        } => {
            boss_jobs::bootstrap::publish_workflows(
                &bases.jobs,
                path,
                owning_team,
                true,
                false,
                Some(SEED_USER),
            )?;
            Ok("ok (published/skipped counts in the log)".to_string())
        }
        Door::Sensors { tenant_id, rows } => {
            let u = url(&bases.jobs, "/api/sensors/batch");
            let resp = refuse(
                client
                    .post(&u)
                    .json(&json!({ "tenant_id": tenant_id, "sensors": rows }))
                    .send()?,
                &format!("POST {u}"),
            )?;
            let body: Value = resp.json().unwrap_or(Value::Null);
            Ok(format!(
                "received {}, inserted {}",
                body.get("received").and_then(Value::as_u64).unwrap_or(0),
                body.get("inserted").and_then(Value::as_u64).unwrap_or(0)
            ))
        }
        Door::PostingRules { tenant_id, rows } => {
            let u = url(&bases.ledger, "/api/ledger/posting-rules/batch");
            let resp = refuse(
                client
                    .post(&u)
                    .json(&json!({ "tenant_id": tenant_id, "rules": rows }))
                    .send()?,
                &format!("POST {u}"),
            )?;
            rules_outcome(resp, &u)
        }
        Door::ProjectionRules { rows } => {
            let u = url(&bases.ledger, "/api/ledger/fact-projection-rules/batch");
            let resp = refuse(
                client.post(&u).json(&json!({ "rules": rows })).send()?,
                &format!("POST {u}"),
            )?;
            rules_outcome(resp, &u)
        }
        Door::Rules { tenant_id, rules } => {
            let source = format!("tenant:{tenant_id}");
            let lines = rules
                .iter()
                .map(|rule| publish_rule(client, &bases.dispatcher, &source, rule))
                .collect::<Result<Vec<_>>>()?;
            Ok(lines.join("; "))
        }
    }
}

/// The ledger doors' own outcome: counts, plus every kept row whose
/// declaration differs from the registry's — named on the line, so an
/// edit under a kept version never reads as "nothing to do".
fn rules_outcome(resp: reqwest::blocking::Response, u: &str) -> Result<String> {
    let out: boss_ledger::posting_rules::BatchOutcome = resp
        .json()
        .with_context(|| format!("POST {u}: the outcome did not parse"))?;
    let mut line = format!("received {}, inserted {}", out.received, out.inserted);
    if !out.differs.is_empty() {
        line.push_str("; differs: ");
        line.push_str(&out.differs.join("; "));
    }
    Ok(line)
}

/// One rule through the dispatcher's authoring door (backlog
/// 458971ef), the seed's own contract for product rules applied at the
/// tenant's: `_validate` first (the dispatcher's rule language, not
/// this build's — a refusal is its words and no draft exists to sit
/// armed for the next publish), then the name's versions, then:
///
/// - a version above the file's → left alone, said so (never walked
///   back; an operator's live edit survives);
/// - the file's version present, at any status → no-op, said so — and
///   if the stored content differs from the file's, the field is named
///   with "bump `version`", because a silent edit under an unchanged
///   version would otherwise read as `present` forever;
/// - otherwise → draft + publish, at the file's version, `source`
///   riding the draft; the promoted row is checked to be the one just
///   drafted (publish promotes the NEWEST draft, and an operator's
///   armed draft would be it otherwise).
///
/// A name another source owns is refused by the door (400, naming
/// both owners) — read here from the versions first, so the refusal
/// names the owner before a draft is even attempted.
fn publish_rule(
    client: &Client,
    dispatcher_base: &str,
    source: &str,
    rule: &boss_dispatcher::rules::registry::RawRule,
) -> Result<String> {
    use boss_dispatcher::rules::authoring::source_label;
    let name = rule.name.as_str();
    let want = u64::from(rule.version);

    let u = url(dispatcher_base, "/api/dispatcher/rules/_validate");
    let resp = refuse(
        client.post(&u).json(rule).send()?,
        &format!("POST {u} ({name})"),
    )?;
    let verdict: Value = resp
        .json()
        .with_context(|| format!("POST {u} ({name}): the verdict did not parse"))?;
    if verdict["ok"] != Value::Bool(true) {
        bail!(
            "{name}: the dispatcher refused the rule: {}",
            verdict["error"].as_str().unwrap_or("(no error given)")
        );
    }

    let u = url(
        dispatcher_base,
        &format!("/api/dispatcher/rules/{name}/versions"),
    );
    let resp = refuse(client.get(&u).send()?, &format!("GET {u}"))?;
    let versions: Vec<Value> = resp
        .json()
        .with_context(|| format!("GET {u}: the versions did not parse"))?;
    if let Some(other) = versions
        .iter()
        .find(|v| v["source"].as_str() != Some(source))
    {
        bail!(
            "{name}: owned by {}; a rule from {source} cannot supersede it — declare it under a \
             name of your own",
            source_label(other["source"].as_str())
        );
    }
    let max = versions.iter().filter_map(|v| v["version"].as_u64()).max();
    if let Some(max) = max.filter(|m| *m > want) {
        return Ok(format!(
            "{name} v{want}: registry ahead at v{max}, left alone"
        ));
    }
    if let Some(stored) = versions.iter().find(|v| v["version"] == json!(want)) {
        let status = stored["status"].as_str().unwrap_or("?");
        let file = serde_json::to_value(rule).unwrap_or(Value::Null);
        let differs: Vec<&str> = ["on_event", "schedule", "when", "do", "delay"]
            .into_iter()
            .filter(|f| {
                file.get(f).cloned().unwrap_or(Value::Null)
                    != stored.get(f).cloned().unwrap_or(Value::Null)
            })
            .collect();
        return Ok(if differs.is_empty() {
            format!("{name} v{want}: present ({status})")
        } else {
            format!(
                "{name} v{want}: present ({status}), differs on {} — kept as published; bump \
                 `version` to supersede",
                differs.join(", ")
            )
        });
    }

    let mut draft = serde_json::to_value(rule).unwrap_or(Value::Null);
    if let Some(obj) = draft.as_object_mut() {
        obj.insert("source".into(), Value::String(source.to_string()));
    }
    let u = url(dispatcher_base, "/api/dispatcher/rules");
    let resp = refuse(
        client.post(&u).json(&draft).send()?,
        &format!("POST {u} ({name})"),
    )?;
    let drafted: Value = resp
        .json()
        .with_context(|| format!("POST {u} ({name}): the draft did not parse"))?;
    if drafted["version"] != json!(want) {
        bail!(
            "{name}: the draft landed at v{} where the file says v{want} — refusing to publish \
             a version the file does not name",
            drafted["version"]
        );
    }
    let u = url(
        dispatcher_base,
        &format!("/api/dispatcher/rules/{name}/publish"),
    );
    let resp = refuse(client.post(&u).send()?, &format!("POST {u}"))?;
    let promoted: Value = resp
        .json()
        .with_context(|| format!("POST {u}: the promoted row did not parse"))?;
    if promoted["version"] != json!(want) || promoted["status"] != json!("active") {
        bail!(
            "{name}: publish promoted v{} ({}) rather than the v{want} just drafted — an armed \
             draft from another author?",
            promoted["version"],
            promoted["status"]
        );
    }
    Ok(match max {
        Some(prev) => format!("{name} v{want}: published, superseding v{prev}"),
        None => format!("{name} v{want}: published"),
    })
}

/// Run the plan against `bases`, one line per step through `out` as
/// each lands. Refuses a plan with any Refused step BEFORE the first
/// write. Blocking HTTP — call from `spawn_blocking`.
pub fn publish(plan: &Plan, bases: &Bases, out: &mut dyn FnMut(String)) -> Result<()> {
    if !plan.publishable() {
        for s in &plan.steps {
            out(plan.render_step(s, None));
        }
        bail!("{}", plan.render_footer());
    }
    let client = seed_client()?;
    let roster_len: usize = plan
        .steps
        .iter()
        .filter_map(|s| match &s.action {
            Action::Write(Door::People { roster }) => Some(roster.len()),
            _ => None,
        })
        .sum();
    for step in &plan.steps {
        let outcome = match &step.action {
            Action::Write(door) => {
                if matches!(door, Door::Workflows { .. }) && roster_len > 0 {
                    wait_for_people_projection(&client, &bases.people, roster_len);
                }
                Some(
                    send(&client, bases, door)
                        .with_context(|| format!("{} ({})", step.path, door.label()))?,
                )
            }
            Action::Skip(_) | Action::Refused(_) => None,
        };
        out(plan.render_step(step, outcome.as_deref()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use boss_testing::scratch::{create_dir, scratch_dir, write_file};
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};

    // ------------------------------------------------------------------
    // A fixture in the real tenant's shape (its names swapped for a
    // neutral tenant: the vocabulary ratchet counts a real tenant's
    // name here; the check-found defects fixed): every contract file
    // publish sends, plus the two nothing reads.
    // ------------------------------------------------------------------

    fn put(dir: &Path, rel: &str, body: &str) {
        let path = dir.join(rel);
        if let Some(parent) = path.parent() {
            create_dir(parent);
        }
        write_file(&path, body);
    }

    const WORKFLOWS: &str = r#"[[workflow]]
kind = "receive-a-sponsorship"
label = "Receive a sponsorship"
category = "sales"
subject_kinds = ["account"]

[[workflow.step]]
title = "received"
kind = "trigger"
ready_when = "true"
title_template = "Stripe reported a payment"
metadata_defaults = { trigger_kind = "event", trigger_name = "stripe-checkout-completed" }

[[workflow.step]]
title = "reconcile"
kind = "task"
ready_when = "steps.received.done"
audience = { individual = "agent-claude" }
title_template = "Match the payment"
fields = [{ name = "stripe_event_id", field_type = "string", required = true }]

[[workflow.step]]
title = "sponsored"
kind = "outcome"
ready_when = "steps.reconcile.done"
title_template = "Sponsorship recognized"
metadata_defaults = { outcome_kind = "completed" }
terminal = { outcome = "sponsored" }
"#;

    fn real_shape(name: &str) -> PathBuf {
        let dir = scratch_dir(&format!("boss-cli-tenant-publish-{name}"));
        put(
            &dir,
            "tenant.toml",
            "[meta]\ntenant_id = \"acme\"\ndisplay_name = \"Acme, LLC\"\n",
        );
        put(&dir, "README.md", "# prose\n");
        put(
            &dir,
            "seeds/classes.json",
            r#"[
  {"subject_kind": "employee", "code": "engineering", "display_name": "Engineering", "member_attribute": "department", "sort_order": 10},
  {"subject_kind": "employee", "code": "founder", "display_name": "Founder", "member_attribute": "role", "sort_order": 1}
]"#,
        );
        put(
            &dir,
            "seeds/employees.json",
            r#"[{"id": "emp-david", "name": "David Auld", "email": "david@acme.example",
  "role": "founder", "department": "engineering", "skill_level": null,
  "hire_date": "2026-09-16", "location": null, "manager_id": null,
  "employment_type": "full-time", "status": "active", "skills": [], "certifications": [],
  "annual_salary_cents": 0},
  {"id": "emp-two", "name": "Second Person", "email": "two@acme.example",
  "role": "founder", "department": "engineering", "skill_level": null,
  "hire_date": "2026-09-16", "location": null, "manager_id": "emp-david",
  "employment_type": "full-time", "status": "active", "skills": [], "certifications": [],
  "annual_salary_cents": 0}]"#,
        );
        put(
            &dir,
            "seeds/policy_rules.toml",
            "[[grants]]\nrole = \"founder\"\nresources = [\"job\", \"step\"]\nactions = [\"read\", \"create\"]\nscope = \"all\"\n",
        );
        put(
            &dir,
            "seeds/business_calendars.json",
            r#"[{"code": "acme-founder", "name": "founder hours", "weekend": [5, 6], "closed": ["2026-12-25"]}]"#,
        );
        put(
            &dir,
            "seeds/locations.toml",
            "[[location]]\nid = \"loc-acme-hq\"\nname = \"HQ\"\nkind = \"office\"\ntimezone = \"America/Los_Angeles\"\n",
        );
        // The real tenant's agent, in the contract's shape (f56155f0).
        put(
            &dir,
            "seeds/agents.toml",
            "[[agent]]\nid = \"agent-claude\"\ndisplay_name = \"Claude (engineering)\"\n\
             default_model = \"opus-5[1m]\"\naliases = [\"claude@acme.example\"]\n",
        );
        // The real tenant's chart (design 18cf4272): 1000 collides with
        // the starter's Cash, 1010 with Cash in Transit; 4200 is new.
        put(
            &dir,
            "seeds/chart_of_accounts.toml",
            "[[account]]\ncode = \"1000\"\nname = \"Bank\"\nkind = \"asset\"\nnormal_balance = \"debit\"\n\
             [[account]]\ncode = \"1010\"\nname = \"Stripe balance\"\nkind = \"asset\"\n\
             normal_balance = \"debit\"\nparent = \"1000\"\n\
             [[account]]\ncode = \"4200\"\nname = \"Support revenue\"\nkind = \"revenue\"\n\
             normal_balance = \"credit\"\n",
        );
        put(&dir, "seeds/workflows.toml", WORKFLOWS);
        // The first sensor (design 14c9b2ad): opens the workflow above.
        put(
            &dir,
            "seeds/sensors.toml",
            "[[sensor]]\nid = \"stripe-sponsorships\"\nsource = \"stripe\"\n\
             credential = \"stripe-restricted-read\"\nevery_minutes = 15\n\
             opens = \"receive-a-sponsorship\"\nsubject_kind = \"custom\"\n",
        );
        // The ledger's two rule files (backlog a40541cb): the
        // sponsorship's posting rule and the projection that turns the
        // recognize step's completion into that fact.
        put(
            &dir,
            "seeds/posting_rules.toml",
            "[[posting_rule]]\nfact_kind = \"finance.sponsorship.received\"\nbasis = \"cash\"\n\
             lines = [\n\
               { account_code = \"1010\", side = \"debit\", amount_path = \"/metadata/amount_cents\" },\n\
               { account_code = \"4100\", side = \"credit\", amount_path = \"/metadata/amount_cents\" },\n\
             ]\n",
        );
        put(
            &dir,
            "seeds/fact_projection_rules.toml",
            "[[projection]]\nevent_kind = \"step.done.task\"\n\
             when = { \"/workflow_kind\" = \"receive-a-sponsorship\", \"/spec_slug\" = \"recognize\" }\n\
             fact_kind = \"finance.sponsorship.received\"\nsource_table = \"jobs\"\n\
             source_id_path = \"/job_id\"\nhappened_on_path = \"/completed_on\"\n",
        );
        // The first reactor (backlog 458971ef), at v2 so the door's
        // "the file's version, not MAX + 1" is exercised. The real
        // tenant's rule names the sibling car's handler; the fixture
        // uses one every build has, to prove the mechanics.
        put(&dir, "seeds/rules.toml", &rules_toml(2, "messages.notify"));
        dir
    }

    fn rules_toml(version: u32, handler: &str) -> String {
        format!(
            "[[rule]]\nname = \"complete-site-live-on-converge-closed\"\n\
             why = \"\"\"\nA cross-protocol reactor: the converge packet's close is the site's evidence.\n\"\"\"\n\
             version = {version}\non_event = \"jobs.job.closed\"\n\
             when = 'kind = \"maintenance-cluster-converge\"'\n\
             [[rule.do]]\nhandler = \"{handler}\"\n"
        )
    }

    fn step<'a>(p: &'a Plan, path: &str) -> &'a Step {
        p.steps
            .iter()
            .find(|s| s.path == path)
            .unwrap_or_else(|| panic!("{path} has a step:\n{:#?}", p.steps))
    }

    // ------------------------------------------------------------------
    // The plan: the contract's files in the engines' order, every
    // present file named, no HTTP.
    // ------------------------------------------------------------------

    #[test]
    fn the_plan_walks_the_doors_in_dependency_order_and_names_every_file() {
        let dir = real_shape("plan-order");
        let p = plan(&dir).unwrap();
        assert_eq!(p.tenant_id, "acme");
        let writes: Vec<&str> = p
            .steps
            .iter()
            .filter(|s| matches!(s.action, Action::Write(_)))
            .map(|s| s.path.as_str())
            .collect();
        assert_eq!(
            writes,
            [
                "seeds/classes.json",
                "seeds/chart_of_accounts.toml",
                "seeds/locations.toml",
                "seeds/business_calendars.json",
                "tenant.toml",
                "seeds/policy_rules.toml",
                "seeds/employees.json",
                "seeds/agents.toml",
                "seeds/workflows.toml",
                "seeds/sensors.toml",
                "seeds/posting_rules.toml",
                "seeds/fact_projection_rules.toml",
                "seeds/rules.toml",
            ],
            "classes → chart of accounts → locations → calendars → company → policy → people → agents → workflows → sensors → posting rules → projections → rules LAST"
        );
        // The chart door (backlog 41af5195): the rows as the ledger
        // door's own type, after the classes.
        match &step(&p, "seeds/chart_of_accounts.toml").action {
            Action::Write(Door::Chart { rows }) => {
                assert_eq!(rows.len(), 3);
                assert_eq!(rows[0].code, "1000");
                assert_eq!(rows[1].parent.as_deref(), Some("1000"));
                assert_eq!(rows[2].kind, "revenue");
            }
            other => panic!("{other:?}"),
        }
        // The rules door (backlog 458971ef): the declarations as the
        // dispatcher's own RawRule, last of all — a reactor fires on
        // the protocols published before it.
        match &step(&p, "seeds/rules.toml").action {
            Action::Write(Door::Rules { tenant_id, rules }) => {
                assert_eq!(tenant_id, "acme");
                assert_eq!(rules.len(), 1);
                assert_eq!(rules[0].name, "complete-site-live-on-converge-closed");
                assert_eq!(rules[0].version, 2);
                assert_eq!(rules[0].on_event.as_deref(), Some("jobs.job.closed"));
                assert_eq!(rules[0].do_steps[0].handler, "messages.notify");
            }
            other => panic!("{other:?}"),
        }
        // The agents door (backlog f56155f0): the declarations as the
        // batch endpoint's rows, before the Workflows whose steps may
        // name an agent as their audience.
        match &step(&p, "seeds/agents.toml").action {
            Action::Write(Door::Agents { rows }) => {
                assert_eq!(rows.len(), 1);
                assert_eq!(rows[0].id, "agent-claude");
                assert_eq!(rows[0].default_model, "opus-5[1m]");
                assert_eq!(rows[0].aliases, ["claude@acme.example"]);
            }
            other => panic!("{other:?}"),
        }
        match &step(&p, "seeds/classes.json").action {
            Action::Write(Door::Classes { rows }) => assert_eq!(rows.len(), 2),
            other => panic!("{other:?}"),
        }
        // The locations door (backlog 1ec8312a): the rows go as the
        // batch endpoint's JSON array, before the roster that FKs
        // into them.
        match &step(&p, "seeds/locations.toml").action {
            Action::Write(Door::Locations { rows }) => {
                assert_eq!(rows.len(), 1);
                assert_eq!(rows[0]["id"], "loc-acme-hq");
                assert_eq!(rows[0]["timezone"], "America/Los_Angeles");
            }
            other => panic!("{other:?}"),
        }
        match &step(&p, "tenant.toml").action {
            Action::Write(Door::Company { id, label }) => {
                assert_eq!(id, "acme");
                assert_eq!(label, "Acme, LLC");
            }
            other => panic!("{other:?}"),
        }
        match &step(&p, "seeds/policy_rules.toml").action {
            // 1 role x 2 resources x 2 actions, expanded by the loader.
            Action::Write(Door::Policy { rules, .. }) => assert_eq!(*rules, 4),
            other => panic!("{other:?}"),
        }
        match &step(&p, "seeds/workflows.toml").action {
            Action::Write(Door::Workflows {
                owning_team, kinds, ..
            }) => {
                assert_eq!(owning_team, "acme");
                assert_eq!(kinds, &["receive-a-sponsorship".to_string()]);
            }
            other => panic!("{other:?}"),
        }
        match &step(&p, "seeds/sensors.toml").action {
            Action::Write(Door::Sensors { tenant_id, rows }) => {
                assert_eq!(tenant_id, "acme");
                assert_eq!(rows.len(), 1);
                assert_eq!(rows[0].id, "stripe-sponsorships");
                assert_eq!(rows[0].opens, "receive-a-sponsorship");
            }
            other => panic!("{other:?}"),
        }
        // The ledger doors (backlog a40541cb): the rows as the batch
        // endpoints take them, the tenant named as the rule's source.
        match &step(&p, "seeds/posting_rules.toml").action {
            Action::Write(Door::PostingRules { tenant_id, rows }) => {
                assert_eq!(tenant_id, "acme");
                assert_eq!(rows.len(), 1);
                assert_eq!(rows[0].fact_kind, "finance.sponsorship.received");
                assert_eq!(rows[0].version, 1);
                assert_eq!(rows[0].basis, "cash");
            }
            other => panic!("{other:?}"),
        }
        match &step(&p, "seeds/fact_projection_rules.toml").action {
            Action::Write(Door::ProjectionRules { rows }) => {
                assert_eq!(rows.len(), 1);
                assert_eq!(rows[0].event_kind, "step.done.task");
                assert_eq!(rows[0].when.as_ref().unwrap()["/spec_slug"], "recognize");
            }
            other => panic!("{other:?}"),
        }
        assert!(p.publishable());
        // Prose is not a file the plan names.
        assert!(!p.steps.iter().any(|s| s.path == "README.md"));
    }

    #[test]
    fn a_file_with_no_reader_is_skipped_by_name_never_silently() {
        let dir = real_shape("plan-skips");
        // The one contract file still without a reader (locations
        // gained its door on 2026-09-17, backlog 1ec8312a).
        put(
            &dir,
            "seeds/subject_kinds.toml",
            "[[subject_kind]]\nkind = \"recipe\"\nlabel = \"Recipe\"\n",
        );
        let p = plan(&dir).unwrap();
        match &step(&p, "seeds/subject_kinds.toml").action {
            Action::Skip(why) => assert!(why.contains("no reader"), "{why}"),
            other => panic!("{other:?}"),
        }
        assert!(
            matches!(
                step(&p, "seeds/locations.toml").action,
                Action::Write(Door::Locations { .. })
            ),
            "locations are written, not skipped: {:?}",
            step(&p, "seeds/locations.toml").action
        );
        // A file the contract does not name at all (agents.toml was
        // this until f56155f0, 2026-09-17).
        put(&dir, "seeds/notes.toml", "[[note]]\ntext = \"x\"\n");
        let p = plan(&dir).unwrap();
        match &step(&p, "seeds/notes.toml").action {
            Action::Skip(why) => {
                assert!(why.contains("no reader"), "{why}");
                assert!(why.contains("not named by the contract"), "{why}");
            }
            other => panic!("{other:?}"),
        }
        assert!(
            matches!(
                step(&p, "seeds/agents.toml").action,
                Action::Write(Door::Agents { .. })
            ),
            "agents are written, not skipped: {:?}",
            step(&p, "seeds/agents.toml").action
        );
        // An engine-only file names its engine, not "no reader".
        put(&dir, "seeds/vendors.toml", "[[vendor]]\nid = \"v\"\n");
        let p = plan(&dir).unwrap();
        match &step(&p, "seeds/vendors.toml").action {
            Action::Skip(why) => {
                assert!(why.contains("read by boss-"), "{why}");
                assert!(!why.contains("no reader"), "{why}");
            }
            other => panic!("{other:?}"),
        }
        let rendered = p.render_step(step(&p, "seeds/notes.toml"), None);
        assert!(rendered.contains("skipped:"), "{rendered}");
    }

    #[test]
    fn a_directory_that_fails_check_is_refused_whole_with_the_plan_still_printed() {
        let dir = real_shape("plan-refused");
        // The real tenant's first draft: a grant action the loader
        // does not know.
        put(
            &dir,
            "seeds/policy_rules.toml",
            "[[grants]]\nrole = \"founder\"\nresource = \"job\"\naction = \"write\"\nscope = \"all\"\n",
        );
        let p = plan(&dir).unwrap();
        assert!(!p.publishable());
        match &step(&p, "seeds/policy_rules.toml").action {
            Action::Refused(why) => assert!(why.contains("write"), "{why}"),
            other => panic!("{other:?}"),
        }
        // Every other write is still planned and visible.
        assert!(matches!(
            step(&p, "seeds/workflows.toml").action,
            Action::Write(_)
        ));
        assert!(
            p.render_footer().contains("REFUSED"),
            "{}",
            p.render_footer()
        );

        // publish() refuses before the first write: a base nothing
        // listens on would fail loudly if anything were sent.
        let bases = Bases::resolve(Some("http://127.0.0.1:1"));
        let mut lines = Vec::new();
        let err = publish(&p, &bases, &mut |l| lines.push(l)).unwrap_err();
        assert!(err.to_string().contains("REFUSED"), "{err}");
        assert!(
            lines.iter().any(|l| l.contains("REFUSED: ")),
            "the refusal names the file:\n{}",
            lines.join("\n")
        );

        // A missing required file is a refusal too.
        let dir = scratch_dir("boss-cli-tenant-publish-plan-missing");
        put(&dir, "tenant.toml", "[meta]\ntenant_id = \"t\"\n");
        let p = plan(&dir).unwrap();
        match &step(&p, "seeds/workflows.toml").action {
            Action::Refused(why) => assert!(why.contains("missing"), "{why}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_gateway_collapses_every_base_and_the_default_is_each_services_port() {
        let g = Bases::resolve(Some("http://gw:8080/"));
        assert_eq!(g.classes, "http://gw:8080");
        assert_eq!(g.jobs, "http://gw:8080");
        assert_eq!(g.people, g.policy);
        let d = Bases::resolve(None);
        assert_eq!(d.jobs, boss_ports::url("jobs"));
        assert_eq!(d.classes, boss_ports::url("classes"));
        assert_eq!(d.ledger, boss_ports::url("ledger"));
        assert_ne!(d.jobs, d.classes, "solo ports, not one base");
        assert!(d.describe(None).contains("boss_ports"));
        assert!(g.describe(Some("http://gw:8080")).contains("gateway"));
    }

    // ------------------------------------------------------------------
    // The doors, against a recording stub that keeps state — so a
    // second run can be judged to write nothing new.
    // ------------------------------------------------------------------

    #[derive(Default)]
    struct Stub {
        /// Every request: (method, path, lowercased head, body).
        log: Vec<(String, String, String, String)>,
        /// What "exists" server-side: policy rule ids, employee ids,
        /// published workflow kinds, design Job ids.
        policy: BTreeSet<String>,
        people: BTreeMap<String, Value>,
        workflows: BTreeSet<String>,
        jobs: usize,
        /// Published sensor ids (insert-if-absent, like the door).
        sensors: BTreeSet<String>,
        /// Published location ids (insert-if-absent, like the door).
        locations: BTreeSet<String>,
        /// Registered agents: id -> display_name (insert-if-absent;
        /// the stub's "migration" pre-registers agent-claude under
        /// the platform's own name, as the real schema does).
        agents: BTreeMap<String, String>,
        /// Published posting rules, "fact_kind v<n>" (insert-if-absent).
        posting_rules: BTreeSet<String>,
        /// Published projections, "event_kind when" (insert-if-absent).
        projections: BTreeSet<String>,
        /// The chart of accounts: code -> name (insert-if-absent by
        /// code; a test pre-seeds the starter's rows the way
        /// 40-ledger.sql does).
        accounts: BTreeMap<String, String>,
        /// The dispatcher's rule registry, one row per (name, version):
        /// the stored draft/active/retired rows with their `source`,
        /// the append-only shape `dispatcher_rules` has.
        rules: Vec<Value>,
    }

    impl Stub {
        fn writes(&self) -> usize {
            self.policy.len()
                + self.people.len()
                + self.workflows.len()
                + self.jobs
                + self.sensors.len()
                + self.locations.len()
                + self.agents.len()
                + self.posting_rules.len()
                + self.projections.len()
                + self.accounts.len()
                + self.rules.len()
        }

        fn active_rule(&self, name: &str) -> Option<&Value> {
            self.rules
                .iter()
                .find(|r| r["name"] == name && r["status"] == "active")
        }
    }

    /// The dispatcher's authoring door, as the real one behaves
    /// (boss_dispatcher::rules::authoring): `_validate` answers
    /// `{ok, error}`; a draft lands at max(declared, MAX + 1) carrying
    /// its `source`, refused when another source owns the name;
    /// publish promotes the newest draft and retires the incumbent.
    fn route_dispatcher(st: &mut Stub, method: &str, path: &str, body: &str) -> (u16, String) {
        let seg = |i: usize| path.split('/').nth(i).unwrap_or("").to_string();
        match (method, path) {
            ("POST", "/api/dispatcher/rules/_validate") => {
                let v: Value = serde_json::from_str(body).unwrap_or(Value::Null);
                let when = v["when"].as_str().unwrap_or("");
                if when.contains("(") {
                    (
                        200,
                        json!({"ok": false, "error": format!("rule {}: when: unbalanced parenthesis", v["name"])}).to_string(),
                    )
                } else {
                    (200, json!({"ok": true, "error": null}).to_string())
                }
            }
            ("POST", "/api/dispatcher/rules") => {
                let v: Value = serde_json::from_str(body).unwrap_or(Value::Null);
                let name = v["name"].as_str().unwrap_or("").to_string();
                let source = v["source"].clone();
                if let Some(other) = st
                    .rules
                    .iter()
                    .find(|r| r["name"] == name && r["source"] != source)
                {
                    return (
                        400,
                        format!(
                            "invalid rule: rule `{name}` is owned by {}; a draft from {} cannot supersede it",
                            other["source"].as_str().unwrap_or("product"),
                            source.as_str().unwrap_or("product")
                        ),
                    );
                }
                let max = st
                    .rules
                    .iter()
                    .filter(|r| r["name"] == name)
                    .filter_map(|r| r["version"].as_u64())
                    .max()
                    .unwrap_or(0);
                let version = (max + 1).max(v["version"].as_u64().unwrap_or(1));
                let mut row = v.clone();
                row["version"] = json!(version);
                row["status"] = json!("draft");
                row["created_at"] = json!("2026-09-17T00:00:00Z");
                st.rules.push(row.clone());
                (201, row.to_string())
            }
            ("GET", p) if p.starts_with("/api/dispatcher/rules/") && p.ends_with("/versions") => {
                let name = seg(4);
                let rows: Vec<&Value> = st.rules.iter().filter(|r| r["name"] == name).collect();
                (200, serde_json::to_string(&rows).unwrap())
            }
            ("POST", p) if p.starts_with("/api/dispatcher/rules/") && p.ends_with("/publish") => {
                let name = seg(4);
                let Some(draft_at) = st
                    .rules
                    .iter()
                    .enumerate()
                    .filter(|(_, r)| r["name"] == name && r["status"] == "draft")
                    .max_by_key(|(_, r)| r["version"].as_u64())
                    .map(|(i, _)| i)
                else {
                    return (404, format!("not found: no draft to publish for {name}"));
                };
                for r in st.rules.iter_mut() {
                    if r["name"] == name && r["status"] == "active" {
                        r["status"] = json!("retired");
                    }
                }
                st.rules[draft_at]["status"] = json!("active");
                (200, st.rules[draft_at].to_string())
            }
            _ => (500, format!("stub: unrouted {method} {path}")),
        }
    }

    fn route(st: &mut Stub, method: &str, path: &str, body: &str) -> (u16, String) {
        let seg = |i: usize| path.split('/').nth(i).unwrap_or("").to_string();
        match (method, path) {
            ("POST", "/api/classes/batch") => {
                let n = serde_json::from_str::<Vec<Value>>(body)
                    .map(|v| v.len())
                    .unwrap_or(0);
                (200, json!({"received": n, "inserted": n}).to_string())
            }
            ("POST", "/api/locations/batch") => {
                // The classes batch shape: a bare JSON array of rows.
                let rows = serde_json::from_str::<Vec<Value>>(body).unwrap_or_default();
                let inserted = rows
                    .iter()
                    .filter_map(|r| r["id"].as_str().map(str::to_string))
                    .filter(|id| st.locations.insert(id.clone()))
                    .count();
                (
                    200,
                    json!({"received": rows.len(), "inserted": inserted}).to_string(),
                )
            }
            ("POST", "/api/agents/batch") => {
                // Insert-if-absent by id; a kept row reports which
                // declared fields differ (here: display_name only).
                let rows = serde_json::from_str::<Vec<Value>>(body).unwrap_or_default();
                let mut inserted = 0usize;
                let mut kept = Vec::new();
                for r in &rows {
                    let id = r["id"].as_str().unwrap_or("").to_string();
                    let name = r["display_name"].as_str().unwrap_or("").to_string();
                    match st.agents.get(&id) {
                        Some(have) => kept.push(json!({
                            "id": id,
                            "differs": if *have == name { json!([]) } else { json!(["display_name"]) }
                        })),
                        None => {
                            st.agents.insert(id, name);
                            inserted += 1;
                        }
                    }
                }
                (
                    200,
                    json!({"received": rows.len(), "inserted": inserted, "kept": kept}).to_string(),
                )
            }
            ("POST", "/api/ledger/accounts/batch") => {
                // Insert-if-absent by code; a kept row reports which
                // declared fields differ (here: name only).
                let rows = serde_json::from_str::<Vec<Value>>(body).unwrap_or_default();
                let mut inserted = 0usize;
                let mut kept = Vec::new();
                for r in &rows {
                    let code = r["code"].as_str().unwrap_or("").to_string();
                    let name = r["name"].as_str().unwrap_or("").to_string();
                    match st.accounts.get(&code) {
                        Some(have) => kept.push(json!({
                            "code": code,
                            "differs": if *have == name { json!([]) } else { json!(["name"]) }
                        })),
                        None => {
                            st.accounts.insert(code, name);
                            inserted += 1;
                        }
                    }
                }
                (
                    200,
                    json!({"received": rows.len(), "inserted": inserted, "kept": kept}).to_string(),
                )
            }
            ("POST", "/api/calendar/business-calendars/batch") => (200, "{}".into()),
            ("POST", "/api/subjects/company") => (201, String::new()),
            ("GET", p) if p.starts_with("/api/policy/rules/") => {
                if st.policy.contains(&seg(4)) {
                    (200, "{}".into())
                } else {
                    (404, String::new())
                }
            }
            ("POST", "/api/policy/rules") => {
                let id = serde_json::from_str::<Value>(body)
                    .ok()
                    .and_then(|v| v["rule"]["id"].as_str().map(str::to_string))
                    .unwrap_or_default();
                st.policy.insert(id);
                (201, "{}".into())
            }
            ("GET", "/api/people") => (
                200,
                Value::Array(st.people.values().cloned().collect()).to_string(),
            ),
            ("POST", "/api/people") => {
                let row: Value = serde_json::from_str(body).unwrap_or(Value::Null);
                let id = row["id"].as_str().unwrap_or("").to_string();
                match st.people.entry(id) {
                    std::collections::btree_map::Entry::Occupied(_) => (409, "duplicate".into()),
                    std::collections::btree_map::Entry::Vacant(e) => {
                        e.insert(row);
                        (201, "{}".into())
                    }
                }
            }
            ("GET", p) if p.starts_with("/api/people/") => match st.people.get(&seg(3)) {
                Some(v) => (200, v.to_string()),
                None => (404, String::new()),
            },
            ("PUT", p) if p.starts_with("/api/people/") => {
                let row: Value = serde_json::from_str(body).unwrap_or(Value::Null);
                st.people.insert(seg(3), row);
                (200, "{}".into())
            }
            ("GET", p) if p.starts_with("/api/workflows/") => {
                if st.workflows.contains(&seg(3)) {
                    (200, json!({"authoring_job_id": "job-1"}).to_string())
                } else {
                    (404, String::new())
                }
            }
            ("POST", "/api/jobs") => {
                let v: Value = serde_json::from_str(body).unwrap_or(Value::Null);
                let kind = v["subject"]["id"].as_str().unwrap_or("").to_string();
                st.workflows.insert(kind);
                st.jobs += 1;
                (200, json!({"id": format!("job-{}", st.jobs)}).to_string())
            }
            ("POST", "/api/sensors/batch") => {
                let v: Value = serde_json::from_str(body).unwrap_or(Value::Null);
                assert_eq!(v["tenant_id"], "acme", "the batch names the tenant: {body}");
                let ids: Vec<String> = v["sensors"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|r| r["id"].as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default();
                let inserted = ids
                    .into_iter()
                    .filter(|id| st.sensors.insert(id.clone()))
                    .count();
                (
                    200,
                    json!({"received": v["sensors"].as_array().map_or(0, Vec::len), "inserted": inserted}).to_string(),
                )
            }
            ("POST", "/api/ledger/posting-rules/batch") => {
                let v: Value = serde_json::from_str(body).unwrap_or(Value::Null);
                assert_eq!(v["tenant_id"], "acme", "the batch names the tenant: {body}");
                let rows = v["rules"].as_array().cloned().unwrap_or_default();
                let inserted = rows
                    .iter()
                    .map(|r| format!("{} v{}", r["fact_kind"], r["version"]))
                    .filter(|k| st.posting_rules.insert(k.clone()))
                    .count();
                (
                    200,
                    json!({"received": rows.len(), "inserted": inserted, "differs": []})
                        .to_string(),
                )
            }
            ("POST", "/api/ledger/fact-projection-rules/batch") => {
                let v: Value = serde_json::from_str(body).unwrap_or(Value::Null);
                let rows = v["rules"].as_array().cloned().unwrap_or_default();
                let inserted = rows
                    .iter()
                    .map(|r| format!("{} {}", r["event_kind"], r["when"]))
                    .filter(|k| st.projections.insert(k.clone()))
                    .count();
                (
                    200,
                    json!({"received": rows.len(), "inserted": inserted, "differs": []})
                        .to_string(),
                )
            }
            ("GET", p) if p.starts_with("/api/jobs/") && p.ends_with("/steps") => {
                // No steps to walk: the bootstrap's own walk is
                // boss-jobs' to test; this proves the door is
                // reached with the right identity.
                (200, "[]".into())
            }
            (_, p) if p.starts_with("/api/dispatcher/") => route_dispatcher(st, method, path, body),
            _ => (500, format!("stub: unrouted {method} {path}")),
        }
    }

    /// A stateful HTTP stub on an ephemeral port, routed by `router`.
    /// Reads one request per connection (the head, then
    /// content-length bytes of body), answers with `connection:
    /// close`, and logs every request on the shared state.
    async fn spawn_stub_with<F>(st: Arc<Mutex<Stub>>, router: F) -> String
    where
        F: Fn(&mut Stub, &str, &str, &str) -> (u16, String) + Send + Sync + 'static,
    {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let router = Arc::new(router);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    break;
                };
                let st = st.clone();
                let router = router.clone();
                tokio::spawn(async move {
                    let mut buf = Vec::new();
                    let mut chunk = [0u8; 4096];
                    let head_end = loop {
                        match sock.read(&mut chunk).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => buf.extend_from_slice(&chunk[..n]),
                        }
                        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                            break i + 4;
                        }
                    };
                    let head = String::from_utf8_lossy(&buf[..head_end]).to_lowercase();
                    let len: usize = head
                        .lines()
                        .find_map(|l| l.strip_prefix("content-length:"))
                        .and_then(|v| v.trim().parse().ok())
                        .unwrap_or(0);
                    while buf.len() < head_end + len {
                        match sock.read(&mut chunk).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => buf.extend_from_slice(&chunk[..n]),
                        }
                    }
                    let body = String::from_utf8_lossy(&buf[head_end..head_end + len]).into_owned();
                    let mut first = head.lines().next().unwrap_or("").split_whitespace();
                    let method = first.next().unwrap_or("").to_uppercase();
                    let path = first.next().unwrap_or("/").to_string();
                    let (code, resp_body) = {
                        let mut st = st.lock().unwrap();
                        let r = router(&mut st, &method, &path, &body);
                        st.log.push((method, path, head, body));
                        r
                    };
                    let resp = format!(
                        "HTTP/1.1 {code} X\r\ncontent-type: application/json\r\n\
                         content-length: {}\r\nconnection: close\r\n\r\n{resp_body}",
                        resp_body.len()
                    );
                    let _ = sock.write_all(resp.as_bytes()).await;
                    let _ = sock.shutdown().await;
                });
            }
        });
        format!("http://{addr}")
    }

    async fn spawn_stub(st: Arc<Mutex<Stub>>) -> String {
        spawn_stub_with(st, route).await
    }

    async fn run_publish(p: Plan, base: String) -> Result<Vec<String>> {
        tokio::task::spawn_blocking(move || {
            let bases = Bases::resolve(Some(&base));
            let mut lines = Vec::new();
            publish(&p, &bases, &mut |l| lines.push(l))?;
            Ok(lines)
        })
        .await
        .unwrap()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_publish_sends_every_door_signed_as_tenant_seed_and_never_as_a_sim() {
        let dir = real_shape("doors");
        let p = plan(&dir).unwrap();
        let st = Arc::new(Mutex::new(Stub::default()));
        let base = spawn_stub(st.clone()).await;

        let lines = run_publish(p.clone(), base).await.unwrap();
        let st = st.lock().unwrap();
        let hit = |m: &str, path: &str| {
            st.log
                .iter()
                .filter(|(mm, pp, _, _)| mm == m && pp == path)
                .count()
        };
        assert_eq!(hit("POST", "/api/classes/batch"), 1);
        assert_eq!(
            hit("POST", "/api/locations/batch"),
            1,
            "one batch for the locations file"
        );
        assert_eq!(st.locations.iter().collect::<Vec<_>>(), ["loc-acme-hq"]);
        assert_eq!(
            hit("POST", "/api/ledger/accounts/batch"),
            1,
            "one batch for the chart of accounts"
        );
        assert_eq!(
            st.accounts.keys().collect::<Vec<_>>(),
            ["1000", "1010", "4200"]
        );
        assert_eq!(hit("POST", "/api/calendar/business-calendars/batch"), 1);
        assert_eq!(hit("POST", "/api/subjects/company"), 1);
        assert_eq!(
            hit("POST", "/api/policy/rules"),
            4,
            "1 role x 2 resources x 2 actions"
        );
        assert_eq!(hit("POST", "/api/people"), 2);
        assert_eq!(
            hit("PUT", "/api/people/emp-two"),
            1,
            "the one manager edge is linked"
        );
        assert_eq!(hit("POST", "/api/jobs"), 1, "one design Job per workflow");
        assert_eq!(
            hit("POST", "/api/agents/batch"),
            1,
            "one batch for the agents file"
        );
        assert_eq!(st.agents.keys().collect::<Vec<_>>(), ["agent-claude"]);
        assert_eq!(
            hit("POST", "/api/sensors/batch"),
            1,
            "one batch for the sensors file"
        );
        assert_eq!(
            st.sensors.iter().collect::<Vec<_>>(),
            ["stripe-sponsorships"]
        );
        assert_eq!(hit("POST", "/api/ledger/posting-rules/batch"), 1);
        assert_eq!(
            st.posting_rules.iter().collect::<Vec<_>>(),
            ["\"finance.sponsorship.received\" v1"]
        );
        assert_eq!(hit("POST", "/api/ledger/fact-projection-rules/batch"), 1);
        assert_eq!(st.projections.len(), 1);
        // The rules door (backlog 458971ef): validate, read the
        // versions, draft, publish — one row, active, at the FILE's
        // version, carrying the tenant's identity as its source.
        assert_eq!(hit("POST", "/api/dispatcher/rules/_validate"), 1);
        assert_eq!(hit("POST", "/api/dispatcher/rules"), 1);
        assert_eq!(
            hit(
                "POST",
                "/api/dispatcher/rules/complete-site-live-on-converge-closed/publish"
            ),
            1
        );
        let live = st
            .active_rule("complete-site-live-on-converge-closed")
            .expect("the tenant's rule is active");
        assert_eq!(live["version"], 2, "the file's version, not MAX + 1");
        assert_eq!(live["source"], "tenant:acme");
        assert_eq!(live["on_event"], "jobs.job.closed");
        assert_eq!(live["do"][0]["handler"], "messages.notify");
        assert!(
            live.get("why").is_none(),
            "why is authoring metadata and never rides the wire: {live}"
        );
        assert_eq!(st.people["emp-two"]["manager_id"], json!("emp-david"));
        // Order on the wire: classes before people, people before the
        // design Job.
        let pos = |m: &str, path: &str| {
            st.log
                .iter()
                .position(|(mm, pp, _, _)| mm == m && pp == path)
                .unwrap()
        };
        assert!(pos("POST", "/api/classes/batch") < pos("POST", "/api/people"));
        assert!(
            pos("POST", "/api/classes/batch") < pos("POST", "/api/ledger/accounts/batch"),
            "the chart goes after the classes (backlog 41af5195)"
        );
        assert!(
            pos("POST", "/api/locations/batch") < pos("POST", "/api/people"),
            "employees.location is a FK into locations, so the sites land first"
        );
        assert!(pos("POST", "/api/people") < pos("POST", "/api/jobs"));
        assert!(
            pos("POST", "/api/agents/batch") < pos("POST", "/api/jobs"),
            "a step's audience may name an agent, so the agents go before the Workflows"
        );
        assert!(
            pos("POST", "/api/jobs") < pos("POST", "/api/sensors/batch"),
            "the sensors name a workflow kind, so the kinds go first"
        );
        assert!(
            pos("POST", "/api/ledger/posting-rules/batch")
                < pos("POST", "/api/ledger/fact-projection-rules/batch"),
            "a projection names the fact kind a posting rule posts, so the rules go first"
        );
        assert!(
            pos("POST", "/api/ledger/fact-projection-rules/batch")
                < pos("POST", "/api/dispatcher/rules"),
            "a reactor may name the fact kind a projection produces, so the projections go before the rules"
        );
        assert!(
            pos("POST", "/api/sensors/batch") < pos("POST", "/api/dispatcher/rules"),
            "a reactor fires on the protocols published before it, so the rules go last"
        );
        // Identity on every request; no sim chain on any.
        for (m, path, head, _) in &st.log {
            assert!(
                head.contains("automation:tenant-seed"),
                "{m} {path} is not signed as automation:tenant-seed:\n{head}"
            );
            assert!(
                !head.contains("x-sim-origin"),
                "{m} {path} carries x-sim-origin — its events would be stamped _simulated and trimmed:\n{head}"
            );
        }
        // The company Subject is the manifest's id + display name.
        let (_, _, _, body) = st
            .log
            .iter()
            .find(|(_, p, _, _)| p == "/api/subjects/company")
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(body).unwrap(),
            json!({"id": "acme", "label": "Acme, LLC"})
        );
        // One line per step, each write with its outcome.
        assert_eq!(lines.len(), p.steps.len(), "{}", lines.join("\n"));
        let people_line = lines
            .iter()
            .find(|l| l.contains("seeds/employees.json"))
            .unwrap();
        assert!(
            people_line.contains("2 posted, 0 already there, 1/1 linked"),
            "{people_line}"
        );
        let classes_line = lines
            .iter()
            .find(|l| l.contains("seeds/classes.json"))
            .unwrap();
        assert!(
            classes_line.contains("received 2, inserted 2"),
            "{classes_line}"
        );
        let chart_line = lines
            .iter()
            .find(|l| l.contains("seeds/chart_of_accounts.toml"))
            .unwrap();
        assert!(
            chart_line.contains("POST /api/ledger/accounts/batch")
                && chart_line.contains("received 3, inserted 3"),
            "{chart_line}"
        );
        let rules_line = lines
            .iter()
            .find(|l| l.contains("seeds/rules.toml"))
            .unwrap();
        assert!(
            rules_line.contains("complete-site-live-on-converge-closed v2: published"),
            "{rules_line}"
        );
    }

    /// A CODE THE STARTER CHART HOLDS IS THE SAME ACCOUNT (backlog
    /// 41af5195; design 18cf4272). The stub pre-seeds 40-ledger.sql's
    /// `1000 Cash` and `1010 Cash in Transit`; the tenant declares
    /// `1000 Bank` and `1010 Stripe balance`. Insert-if-absent keeps
    /// both under the starter's names and the publish line names the
    /// field — the tenant adopts the code or chooses another, never a
    /// silent rename of an account every journal line points at.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_starter_chart_code_is_kept_and_the_differing_name_is_named_in_the_line() {
        let dir = real_shape("chart-kept");
        let p = plan(&dir).unwrap();
        let st = Arc::new(Mutex::new(Stub::default()));
        {
            let mut st = st.lock().unwrap();
            st.accounts.insert("1000".into(), "Cash".into());
            st.accounts.insert("1010".into(), "Cash in Transit".into());
        }
        let base = spawn_stub(st.clone()).await;
        let lines = run_publish(p, base).await.unwrap();
        let chart_line = lines
            .iter()
            .find(|l| l.contains("seeds/chart_of_accounts.toml"))
            .unwrap();
        assert!(
            chart_line.contains("received 3, inserted 1")
                && chart_line.contains("1000 (name differs)")
                && chart_line.contains("1010 (name differs)")
                && chart_line.contains("adopt the code or choose another"),
            "{chart_line}"
        );
        let st = st.lock().unwrap();
        assert_eq!(
            st.accounts["1000"], "Cash",
            "kept as the starter registered it"
        );
        assert_eq!(st.accounts["4200"], "Support revenue");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_second_publish_writes_nothing_new() {
        let dir = real_shape("idempotent");
        let p = plan(&dir).unwrap();
        let st = Arc::new(Mutex::new(Stub::default()));
        let base = spawn_stub(st.clone()).await;

        run_publish(p.clone(), base.clone()).await.unwrap();
        let (after_first, first_len) = {
            let st = st.lock().unwrap();
            (st.writes(), st.log.len())
        };
        assert!(after_first > 0);

        let lines = run_publish(p, base).await.unwrap();
        let st = st.lock().unwrap();
        assert_eq!(st.writes(), after_first, "the second run created nothing");
        let second = &st.log[first_len..];
        let posts = |path: &str| {
            second
                .iter()
                .filter(|(m, p, _, _)| m == "POST" && p == path)
                .count()
        };
        assert_eq!(
            posts("/api/policy/rules"),
            0,
            "every rule GET answered 200, none re-POSTed"
        );
        assert_eq!(
            posts("/api/jobs"),
            0,
            "an operator-published kind is skipped, no new design Job"
        );
        assert_eq!(posts("/api/people"), 2, "the roster is re-POSTed and 409s");
        let people_line = lines
            .iter()
            .find(|l| l.contains("seeds/employees.json"))
            .unwrap();
        assert!(
            people_line.contains("0 posted, 2 already there"),
            "{people_line}"
        );
        // An unchanged rule version is a no-op, and the line says so:
        // validated and read, no draft, no publish.
        assert_eq!(posts("/api/dispatcher/rules"), 0, "no second draft");
        assert_eq!(
            posts("/api/dispatcher/rules/complete-site-live-on-converge-closed/publish"),
            0
        );
        let rules_line = lines
            .iter()
            .find(|l| l.contains("seeds/rules.toml"))
            .unwrap();
        assert!(
            rules_line.contains("complete-site-live-on-converge-closed v2: present (active)"),
            "{rules_line}"
        );
    }

    /// A BUMPED VERSION SUPERSEDES; A LIVE VERSION AHEAD OF THE FILE IS
    /// LEFT ALONE; A NAME ANOTHER SOURCE OWNS IS REFUSED (backlog
    /// 458971ef) — the seed's own contract for product rules, at the
    /// tenant's door. Each answer is in the line, by rule name.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_rule_version_bump_supersedes_and_a_registry_ahead_is_left_alone() {
        let dir = real_shape("rule-versions");
        let st = Arc::new(Mutex::new(Stub::default()));
        let base = spawn_stub(st.clone()).await;
        run_publish(plan(&dir).unwrap(), base.clone())
            .await
            .unwrap();
        assert_eq!(
            st.lock()
                .unwrap()
                .active_rule("complete-site-live-on-converge-closed")
                .unwrap()["version"],
            2
        );

        // Bump: v3 supersedes v2, which is retired, not deleted.
        put(&dir, "seeds/rules.toml", &rules_toml(3, "messages.notify"));
        let lines = run_publish(plan(&dir).unwrap(), base.clone())
            .await
            .unwrap();
        {
            let st = st.lock().unwrap();
            let live = st
                .active_rule("complete-site-live-on-converge-closed")
                .unwrap();
            assert_eq!(live["version"], 3);
            assert_eq!(live["source"], "tenant:acme");
            let retired: Vec<u64> = st
                .rules
                .iter()
                .filter(|r| r["status"] == "retired")
                .filter_map(|r| r["version"].as_u64())
                .collect();
            assert_eq!(retired, [2], "append-only: v2 is retired, not deleted");
        }
        let line = lines
            .iter()
            .find(|l| l.contains("seeds/rules.toml"))
            .unwrap();
        assert!(line.contains("v3: published, superseding v2"), "{line}");

        // The file walks back to v1: the registry is ahead, left alone
        // and SAID so — never silently "present".
        put(&dir, "seeds/rules.toml", &rules_toml(1, "messages.notify"));
        let lines = run_publish(plan(&dir).unwrap(), base.clone())
            .await
            .unwrap();
        let line = lines
            .iter()
            .find(|l| l.contains("seeds/rules.toml"))
            .unwrap();
        assert!(
            line.contains("v1: registry ahead at v3, left alone"),
            "{line}"
        );
        assert_eq!(
            st.lock()
                .unwrap()
                .active_rule("complete-site-live-on-converge-closed")
                .unwrap()["version"],
            3
        );

        // The same version, different content: kept as published, and
        // the line names the field, so a silent edit cannot hide
        // behind "present".
        put(
            &dir,
            "seeds/rules.toml",
            &rules_toml(3, "messages.notify").replace("jobs.job.closed", "jobs.job.created"),
        );
        let lines = run_publish(plan(&dir).unwrap(), base.clone())
            .await
            .unwrap();
        let line = lines
            .iter()
            .find(|l| l.contains("seeds/rules.toml"))
            .unwrap();
        assert!(
            line.contains("v3: present (active), differs on on_event") && line.contains("bump"),
            "{line}"
        );

        // A name the product owns: the door refuses, the publish fails
        // naming both owners, nothing is written.
        st.lock().unwrap().rules.push(json!({
            "name": "auto-park-on-gate-green", "version": 1, "status": "active",
            "on_event": "step.done.gate-verdict", "when": null,
            "do": [{"handler": "jobs.auto-park", "args": {}}], "delay": null,
            "created_at": "2026-09-10T00:00:00Z"
        }));
        put(
            &dir,
            "seeds/rules.toml",
            &rules_toml(2, "messages.notify").replace(
                "complete-site-live-on-converge-closed",
                "auto-park-on-gate-green",
            ),
        );
        let err = run_publish(plan(&dir).unwrap(), base).await.unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("auto-park-on-gate-green"), "{msg}");
        assert!(
            msg.contains("owned by product") && msg.contains("tenant:acme"),
            "{msg}"
        );
        let st = st.lock().unwrap();
        assert_eq!(
            st.rules
                .iter()
                .filter(|r| r["name"] == "auto-park-on-gate-green")
                .count(),
            1,
            "no draft was written under the product's name"
        );
    }

    /// THE DISPATCHER'S `_validate` IS ASKED FIRST, and its refusal
    /// fails the publish in its own words — before a draft exists to
    /// sit armed for the next publish (memory: publish promotes any
    /// draft). `check` catches this offline too; this pins that the
    /// wire's gate is consulted, since the deployed dispatcher's rule
    /// language may differ from the CLI's build.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_rule_the_dispatcher_refuses_fails_the_publish_before_any_draft() {
        let dir = real_shape("rule-refused");
        let p = plan(&dir).unwrap();
        let st = Arc::new(Mutex::new(Stub::default()));
        let base = spawn_stub_with(st.clone(), |st, m, path, body| {
            if m == "POST" && path == "/api/dispatcher/rules/_validate" {
                return (
                    200,
                    json!({"ok": false, "error": "rule complete-site-live-on-converge-closed: when: unknown field `kind`"}).to_string(),
                );
            }
            route(st, m, path, body)
        })
        .await;
        let err = run_publish(p, base).await.unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("rules.toml"), "{msg}");
        assert!(
            msg.contains("unknown field `kind`"),
            "the dispatcher's own words: {msg}"
        );
        let st = st.lock().unwrap();
        assert!(st.rules.is_empty(), "no draft was written: {:?}", st.rules);
    }

    /// A REGISTERED AGENT IS KEPT, AND THE LINE SAYS HOW THE DECLARATION
    /// DIFFERS (backlog f56155f0). The platform's migration registered
    /// `agent-claude` under its own display name; the tenant declares
    /// the same id under another. Insert-if-absent keeps the row, and
    /// the publish line names the field — never a silent "already
    /// there".
    #[tokio::test(flavor = "multi_thread")]
    async fn a_registered_agent_is_kept_and_the_differing_field_is_named_in_the_line() {
        let dir = real_shape("agent-kept");
        let p = plan(&dir).unwrap();
        let st = Arc::new(Mutex::new(Stub::default()));
        st.lock().unwrap().agents.insert(
            "agent-claude".into(),
            "Claude (Claude Code sessions on the dev pod)".into(),
        );
        let base = spawn_stub(st.clone()).await;
        let lines = run_publish(p, base).await.unwrap();
        let agents_line = lines
            .iter()
            .find(|l| l.contains("seeds/agents.toml"))
            .unwrap();
        assert!(
            agents_line.contains("POST /api/agents/batch")
                && agents_line.contains("received 1, inserted 0")
                && agents_line.contains("agent-claude (display_name differs)"),
            "{agents_line}"
        );
        let st = st.lock().unwrap();
        assert_eq!(
            st.agents["agent-claude"], "Claude (Claude Code sessions on the dev pod)",
            "kept as the platform registered it"
        );
    }

    /// THE REAL PEOPLE API SAYS 409 FOR MORE THAN A DUPLICATE ID
    /// (backlog 0d2d7daa, 2026-09-16). `PeopleError::Conflict` — a role
    /// with no active Class, a location not in the registry — maps to
    /// 409 CONFLICT too, and `seed_people` counted every 409 as
    /// "already there": the real company's founder, whose `location`
    /// names a site no door seeds, would have been skipped in silence
    /// and the Workflows published against an empty roster. A 409 is
    /// "already there" only when GET /api/people/{id} says the row
    /// exists — the merge observed, never assumed.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_409_for_a_row_that_does_not_exist_is_a_refusal_not_already_there() {
        let dir = real_shape("409-not-there");
        let p = plan(&dir).unwrap();
        let st = Arc::new(Mutex::new(Stub::default()));
        let base = spawn_stub_with(st.clone(), |st, m, path, body| {
            if m == "POST" && path == "/api/people" {
                let row: Value = serde_json::from_str(body).unwrap_or(Value::Null);
                if row["id"].as_str() == Some("emp-two") {
                    return (
                        409,
                        "location `loc-nowhere` is not an active Location in the registry".into(),
                    );
                }
            }
            route(st, m, path, body)
        })
        .await;
        let err = run_publish(p, base).await.unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("employees.json"), "{msg}");
        assert!(msg.contains("emp-two"), "the row is named: {msg}");
        assert!(
            msg.contains("not an active Location"),
            "the API's own words: {msg}"
        );
        let st = st.lock().unwrap();
        assert!(
            st.log
                .iter()
                .any(|(m, p, _, _)| m == "GET" && p == "/api/people/emp-two"),
            "the 409 was checked against the roster, not assumed"
        );
        assert_eq!(
            st.log
                .iter()
                .filter(|(m, p, _, _)| m == "POST" && p == "/api/jobs")
                .count(),
            0,
            "no design Job opens against a roster that did not land"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_refused_employee_fails_the_publish_before_the_workflows() {
        let dir = real_shape("refused-employee");
        // A role no Class row carries: the people API refuses it.
        put(
            &dir,
            "seeds/employees.json",
            r#"[{"id": "emp-x", "name": "X", "email": "x@acme.example",
  "role": "nobody", "department": "engineering", "skill_level": null,
  "hire_date": "2026-09-16", "location": null, "manager_id": null,
  "employment_type": "full-time", "status": "active", "skills": [], "certifications": [],
  "annual_salary_cents": 0}]"#,
        );
        let p = plan(&dir).unwrap();
        let st = Arc::new(Mutex::new(Stub::default()));
        let base = spawn_stub_with(st.clone(), |st, m, path, body| {
            if m == "POST" && path == "/api/people" {
                let row: Value = serde_json::from_str(body).unwrap_or(Value::Null);
                if row["role"].as_str() == Some("nobody") {
                    return (422, "role `nobody` has no active Class".into());
                }
            }
            route(st, m, path, body)
        })
        .await;
        let err = run_publish(p, base).await.unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("employees.json"), "{msg}");
        assert!(
            msg.contains("no active Class"),
            "the API's own words: {msg}"
        );
        let st = st.lock().unwrap();
        assert_eq!(
            st.log
                .iter()
                .filter(|(m, p, _, _)| m == "POST" && p == "/api/jobs")
                .count(),
            0,
            "no design Job opens against a roster that did not land"
        );
    }
}
