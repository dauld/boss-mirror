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
//! credentials (knowledge about the secrets the deployment holds,
//! never a value; backlog ee368d0c) → sensors (a sensor names a
//! credential by id) → the ledger's posting rules → its event→fact projections
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
//! IDEMPOTENT, LIKE THE ENGINES. Every door inserts if absent: the
//! classes, chart, locations, calendars, credentials, sensors and
//! ledger batches; the company Subject; policy GETs each rule before
//! it POSTs; an employee's 409 is followed by a GET and a comparison;
//! the workflow publish keeps a kind an authoring Job already
//! published; a dispatcher rule at its file's version is `present`. A
//! second run writes nothing new.
//!
//! THE INSTANCE IS THE TRUTH; `--take <registry>` OVERWRITES BY
//! DECISION (design e187198f, David 2026-09-18). Measured that day
//! (agent report on fed7a9e2): four doors overwrote a live row on
//! every republish — business calendars wholesale, the company label,
//! an employee's declared fields, an agent's whole row — and this verb
//! runs at every services-container start, so an operator's edit to
//! any of them lived until the next boot; four other registries
//! (classes, sensors, policy, workflows) kept the live row and said
//! NOTHING when the file differed, so a repo edit that never landed
//! was dead text. The rule of 09887242 ("the tenant's declaration
//! wins on declared fields", 2026-09-17) covered a repo-edited
//! location that had not landed — the bootstrap case — and is dropped:
//! seeds bootstrap an OSS install and the playground; the live
//! instance runs on its data; the repo is bootstrap + export. So every
//! door is insert-if-absent by default, each batch route takes
//! `?mode=insert-if-absent|take` where the semantics live, this verb
//! sends `take` ONLY for the registries named in `--take
//! <registry>[,<registry>]` (the employee overlay PUTs only under
//! `--take employees`; policy's `force` and the workflows' supersede
//! are their doors' takes), every take prints the overwritten rows
//! field by field, and EVERY registry's line names its kept-but-
//! differing rows in one shape — `kept: <id> differs on <fields> (the
//! instance is the truth; --take <registry> overwrites)` — as the
//! decision surface. The contract states it in docs/tenant-contract.md;
//! the decision is in docs/architecture-decisions.md.
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
use boss_core::publish::{
    FieldChange, KeptRow, PublishMode, UpdatedRow, render_kept, render_updated,
};
use boss_core::tenant_manifest::TenantToml;
use reqwest::blocking::Client;
use serde_json::{Value, json};
use tracing::{info, warn};

use crate::tenant::{self, Status};

/// The registries `--take` may name, in the plan's order — each one a
/// door with an overwrite of its own: the calendar and agents batches'
/// `?mode=take`, the company mint's, the employee overlay PUT, the
/// Class edit door (`PUT /api/classes/{kind}/{code}`), policy's
/// `force`, the workflows' supersede. A registry not here has no
/// overwrite (sensors, credentials, locations, the chart, the ledger's
/// rules, the reactors) and its line says so.
pub const TAKEABLE: &[&str] = &[
    "classes",
    "calendars",
    "company",
    "policy",
    "employees",
    "agents",
    "workflows",
];

/// What `--take <registry>[,<registry>]` named: the only registries
/// this publish may overwrite a live row of.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Take(BTreeSet<String>);

impl Take {
    /// Parse the flag's value. Refuses a name no door can take, naming
    /// the ones that can — a typo must not read as "nothing taken".
    pub fn parse(spec: Option<&str>) -> Result<Self> {
        let mut set = BTreeSet::new();
        for name in spec
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            if !TAKEABLE.contains(&name) {
                bail!(
                    "--take {name}: no door overwrites that registry; the registries a take can \
                     name are {}",
                    TAKEABLE.join(", ")
                );
            }
            set.insert(name.to_string());
        }
        Ok(Self(set))
    }

    #[cfg(test)]
    pub fn named(registries: &[&str]) -> Self {
        Self(registries.iter().map(|s| s.to_string()).collect())
    }

    pub fn has(&self, registry: &str) -> bool {
        self.0.contains(registry)
    }

    /// The door's mode for `registry`.
    pub fn mode(&self, registry: &str) -> PublishMode {
        if self.has(registry) {
            PublishMode::Take
        } else {
            PublishMode::InsertIfAbsent
        }
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// For the report header.
    pub fn describe(&self) -> String {
        if self.is_empty() {
            "take: nothing — every door is insert-if-absent; the instance is the truth".to_string()
        } else {
            format!(
                "take: {} — the declaration OVERWRITES live rows of these registries; every \
                 other door is insert-if-absent",
                self.0.iter().cloned().collect::<Vec<_>>().join(", ")
            )
        }
    }
}

/// A batch door's URL with the mode the take decides — the query is
/// omitted for the default so a door that predates the parameter
/// (none today; the shape is for the record) reads the same request.
fn moded(base: &str, path: &str, mode: PublishMode) -> String {
    match mode {
        PublishMode::InsertIfAbsent => url(base, path),
        PublishMode::Take => format!("{}?mode=take", url(base, path)),
    }
}

/// The batch doors' common answer, as this verb reads it: counts plus
/// the rows kept-but-differing and the rows a take updated. Every
/// field defaults so a door that answers a subset (classes, sensors:
/// no `updated`) parses.
#[derive(Debug, Default, serde::Deserialize)]
struct BatchAnswer {
    #[serde(default)]
    received: usize,
    #[serde(default)]
    inserted: usize,
    #[serde(default)]
    kept: Vec<KeptRow>,
    #[serde(default)]
    updated: Vec<UpdatedRow>,
    #[serde(default)]
    unchanged: usize,
}

impl BatchAnswer {
    /// `received N, inserted M[, updated k: …][, n already as
    /// declared][; kept: …]` — one shape for every batch door.
    fn line(&self, take: Option<&str>) -> String {
        let mut s = format!("received {}, inserted {}", self.received, self.inserted);
        if !self.updated.is_empty() {
            s.push_str(", ");
            s.push_str(&render_updated(&self.updated));
        }
        if self.unchanged > 0 {
            s.push_str(&format!(", {} already as declared", self.unchanged));
        }
        let kept = render_kept(&self.kept, take);
        if !kept.is_empty() {
            s.push_str("; ");
            s.push_str(&kept);
        }
        s
    }
}

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
    /// The instance's credential declarations (backlog ee368d0c):
    /// knowledge about each secret the deployment holds — where the
    /// value lives, who reads it — never a value. Before the sensors,
    /// because a sensor names one by id.
    Credentials {
        tenant_id: String,
        rows: Vec<boss_jobs::credentials::CredentialInput>,
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
            Door::People { .. } => {
                "POST /api/people, PUT a row there that differs, then PUT manager links"
            }
            Door::Agents { .. } => "POST /api/agents/batch",
            Door::Workflows { .. } => "POST /api/jobs (workflow-design), walked to publish",
            Door::Credentials { .. } => "POST /api/credentials/batch",
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
            Door::Classes { rows } => format!(
                "{} classes (insert-if-absent; a held row that differs is named; --take classes edits it)",
                rows.len()
            ),
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
            Door::Calendars { count, .. } => format!(
                "{count} calendars (insert-if-absent by code; a held code that differs is named; --take calendars replaces it wholesale)"
            ),
            Door::Company { id, label } => format!(
                "company `{id}` ({label}) (insert-if-absent; a held label that differs is named; --take company overwrites it)"
            ),
            Door::Policy { rules, .. } => format!(
                "{rules} rules (each GET first; a held rule is kept and a differing scope or active is named; --take policy overwrites)"
            ),
            Door::People { roster } => format!(
                "{} people, {} manager links (a row already there is kept and its differing declared fields are named; --take employees applies them)",
                roster.len(),
                manager_split(roster.clone()).1.len()
            ),
            Door::Agents { rows } => format!(
                "{} agents (by id: {}; a registered row is kept and its differing declared fields are named; --take agents applies the whole declaration; an undeclared alias is kept either way)",
                rows.len(),
                rows.iter()
                    .map(|a| a.id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Door::Workflows {
                owning_team, kinds, ..
            } => format!(
                "{} workflows as owning_team `{owning_team}` ({}) (a kind an authoring Job already published is kept and its differing facets are named; --take workflows supersedes it with a new version)",
                kinds.len(),
                kinds.join(", ")
            ),
            Door::Credentials { rows, .. } => format!(
                "{} credentials (insert-if-absent by id: {}; a row already there keeps its rotation book-keeping; locations only, never a value)",
                rows.len(),
                rows.iter()
                    .map(|r| r.id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Door::Sensors { rows, .. } => format!(
                "{} sensors (insert-if-absent by id: {}; a held row that differs is named — no door overwrites a sensor)",
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
    // 9. Credentials (backlog ee368d0c): a sensor names one by id, so
    //    the declarations go before the sensors. Knowledge only — the
    //    loader refuses a row carrying a value under any key.
    door(present(dir, &["seeds/credentials.toml"]), &|p| {
        Ok(Door::Credentials {
            tenant_id: tenant_id.clone(),
            rows: boss_jobs::credentials::load_credentials_toml(p).map_err(anyhow::Error::msg)?,
        })
    })?;
    // 10. Sensors (design 14c9b2ad): each row names the workflow kind
    //    a reading opens, so the kinds go first; the registry row is
    //    what the platform's 5-minute poll reads.
    door(present(dir, &["seeds/sensors.toml"]), &|p| {
        Ok(Door::Sensors {
            tenant_id: tenant_id.clone(),
            rows: boss_jobs::sensors::load_sensors_toml(p).map_err(anyhow::Error::msg)?,
        })
    })?;
    // 11. Posting rules (backlog a40541cb): fact kind → journal lines,
    //     landed with the tenant as their source.
    door(present(dir, &["seeds/posting_rules.toml"]), &|p| {
        Ok(Door::PostingRules {
            tenant_id: tenant_id.clone(),
            rows: boss_ledger::posting_rules::load_posting_rules_toml(p)
                .map_err(anyhow::Error::msg)?,
        })
    })?;
    // 12. Event→fact projections: a projection names the fact kind a
    //     posting rule posts and (through `when`) the workflow whose
    //     step it reads, so both go first.
    door(present(dir, &["seeds/fact_projection_rules.toml"]), &|p| {
        Ok(Door::ProjectionRules {
            rows: boss_ledger::posting_rules::load_projection_rules_toml(p)
                .map_err(anyhow::Error::msg)?,
        })
    })?;
    // 13. Rules LAST (backlog 458971ef): a reactor's `when` and args
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

/// The declared keys of `declared` that `current` (the row as the
/// people API holds it) reads differently — what applying the
/// declaration would change. A key the API's row does not carry is
/// one the door cannot hold (the real tenant's `github_username`),
/// dropped by the POST already, so it is not a difference: comparing
/// it would name a change no PUT can make, on every publish, forever.
fn declared_changes(current: &Value, declared: &Value) -> Vec<FieldChange> {
    let (Some(cur), Some(decl)) = (current.as_object(), declared.as_object()) else {
        return Vec::new();
    };
    decl.iter()
        .filter_map(|(k, want)| {
            let have = cur.get(k)?;
            (have != want).then(|| FieldChange {
                field: k.clone(),
                from: have.clone(),
                to: want.clone(),
            })
        })
        .collect()
}

/// POST every roster row; a row already there is UPDATED on the
/// declared fields that differ; then PUT the manager edges back. Any
/// refusal fails the publish — the Workflows published next assign
/// work to this roster.
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
///
/// THE INSTANCE IS THE TRUTH; `--take employees` APPLIES THE
/// DECLARATION (design e187198f, 2026-09-18). The GET body is compared
/// key by key against the declaration ([`declared_changes`]) and, by
/// default, a row that differs is KEPT and the line names the fields:
/// `kept: emp-david differs on location (the instance is the truth;
/// --take employees overwrites)`. Under `take` a declared key that
/// differs is applied through the door's own update — the GET body
/// with the declared keys overlaid, PUT back, which is the manager-link
/// idiom below and records the door's `people.employee.updated` — so
/// a column the tenant does not declare (a salary set out of band)
/// rides the PUT unchanged, and the line names each change. An
/// explicit `null` in the file IS a declaration (the tenant says: no
/// location); to leave a column alone, omit the key. A refused update
/// fails the publish with the API's words, the same as a refused
/// POST. Idempotent: a row that compares equal is counted `already as
/// declared` and not written, and a manager edge already in place is
/// not re-PUT.
///
/// The rule this replaces — "the tenant's declaration wins on declared
/// fields", backlog 09887242, 2026-09-17 — was measured on a
/// repo-edited location that had not landed (`emp-david` at `loc-hq`
/// while the file said `loc-algedonic-hq`), the bootstrap case; on a
/// running instance the same overlay reverted every operator edit at
/// the next boot, which is the collision the design decided against.
fn seed_people(client: &Client, people_base: &str, roster: &[Value], take: bool) -> Result<String> {
    let (rows, links) = manager_split(roster.to_vec());
    let post_url = url(people_base, "/api/people");
    let (mut posted, mut same) = (0usize, 0usize);
    let mut updated: Vec<UpdatedRow> = Vec::new();
    let mut kept: Vec<KeptRow> = Vec::new();
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
                let got = client
                    .get(&row_url)
                    .send()
                    .with_context(|| format!("GET {row_url} after a 409"))?;
                if !got.status().is_success() {
                    bail!(
                        "POST {post_url} ({id}) → 409 {} — and GET {row_url} says the row is \
                         not there, so this is a refusal, not a duplicate",
                        resp.text().unwrap_or_default()
                    );
                }
                let mut current: Value = got
                    .json()
                    .with_context(|| format!("GET {row_url}: the row did not parse"))?;
                let changes = declared_changes(&current, emp);
                if changes.is_empty() {
                    same += 1;
                    continue;
                }
                if !take {
                    kept.push(KeptRow {
                        id: id.to_string(),
                        differs: changes.into_iter().map(|c| c.field).collect(),
                    });
                    continue;
                }
                if let (Some(cur), Some(decl)) = (current.as_object_mut(), emp.as_object()) {
                    for (k, v) in decl {
                        cur.insert(k.clone(), v.clone());
                    }
                }
                refuse(
                    client.put(&row_url).json(&current).send()?,
                    &format!(
                        "PUT {row_url} (applying the declaration: {})",
                        changes
                            .iter()
                            .map(FieldChange::render)
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                )?;
                updated.push(UpdatedRow {
                    id: id.to_string(),
                    changes,
                });
            }
            s if (200..300).contains(&s) => posted += 1,
            _ => {
                refuse(resp, &format!("POST {post_url} ({id})"))?;
            }
        }
    }
    // Manager links are best-effort per edge — a failed link degrades
    // the org chart, not the publish (the engines' rule). An edge
    // already in place counts as linked and is not written again.
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
        if body.get("manager_id").and_then(Value::as_str) == Some(mgr_id.as_str()) {
            linked += 1;
            continue;
        }
        if let Some(obj) = body.as_object_mut() {
            obj.insert("manager_id".into(), Value::String(mgr_id.clone()));
        }
        match client.put(&row_url).json(&body).send() {
            Ok(r) if r.status().is_success() => linked += 1,
            Ok(r) => warn!(%emp_id, status = %r.status(), "PUT manager link failed"),
            Err(e) => warn!(%emp_id, error = %e, "PUT manager link transport error"),
        }
    }
    let mut line = format!("{posted} posted");
    if !updated.is_empty() {
        line.push_str(", ");
        line.push_str(&render_updated(&updated));
    }
    if same > 0 {
        line.push_str(&format!(", {same} already as declared"));
    }
    line.push_str(&format!(", {linked}/{} linked", links.len()));
    let kept = render_kept(&kept, Some("employees"));
    if !kept.is_empty() {
        line.push_str("; ");
        line.push_str(&kept);
    }
    Ok(line)
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

/// `--take classes`: the Class registry's batch has no take of its
/// own (insert-if-absent by design, so a seed cannot clobber an edit),
/// but its edit door does — `PUT /api/classes/{kind}/{code}`, the
/// operator's own path. Each row the batch KEPT-but-differing is read
/// back, the declared body is PUT through that door, and the change is
/// named field by field from the row read.
fn take_classes(
    client: &Client,
    classes_base: &str,
    rows: &[Value],
    kept: &[KeptRow],
) -> Result<Vec<UpdatedRow>> {
    let mut updated = Vec::new();
    for k in kept.iter().filter(|k| !k.differs.is_empty()) {
        let Some(declared) = rows.iter().find(|r| {
            format!(
                "{}/{}",
                r.get("subject_kind").and_then(Value::as_str).unwrap_or(""),
                r.get("code").and_then(Value::as_str).unwrap_or("")
            ) == k.id
        }) else {
            bail!(
                "the classes door named a kept row this batch did not send: {}",
                k.id
            );
        };
        let row_url = url(classes_base, &format!("/api/classes/{}", k.id));
        let current: Value = refuse(client.get(&row_url).send()?, &format!("GET {row_url}"))?
            .json()
            .with_context(|| format!("GET {row_url}: the row did not parse"))?;
        let changes: Vec<FieldChange> = k
            .differs
            .iter()
            .map(|f| FieldChange {
                field: f.clone(),
                from: current.get(f).cloned().unwrap_or(Value::Null),
                to: declared.get(f).cloned().unwrap_or(Value::Null),
            })
            .collect();
        refuse(
            client.put(&row_url).json(declared).send()?,
            &format!(
                "PUT {row_url} (taking: {})",
                changes
                    .iter()
                    .map(FieldChange::render)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        )?;
        updated.push(UpdatedRow {
            id: k.id.clone(),
            changes,
        });
    }
    Ok(updated)
}

/// Send one door. Returns the outcome line's tail.
fn send(client: &Client, bases: &Bases, door: &Door, take: &Take) -> Result<String> {
    match door {
        Door::Classes { rows } => {
            let u = url(&bases.classes, "/api/classes/batch");
            let resp = refuse(client.post(&u).json(rows).send()?, &format!("POST {u}"))?;
            let mut out: BatchAnswer = resp
                .json()
                .with_context(|| format!("POST {u}: the outcome did not parse"))?;
            if take.has("classes") {
                out.updated = take_classes(client, &bases.classes, rows, &out.kept)?;
                out.kept.clear();
            }
            Ok(out.line(Some("classes")))
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
        Door::Calendars { body, .. } => {
            let u = moded(
                &bases.calendar,
                "/api/calendar/business-calendars/batch",
                take.mode("calendars"),
            );
            let resp = refuse(
                client.post(&u).body(body.clone()).send()?,
                &format!("POST {u}"),
            )?;
            let out: BatchAnswer = resp
                .json()
                .with_context(|| format!("POST {u}: the outcome did not parse"))?;
            Ok(out.line(Some("calendars")))
        }
        Door::Company { id, label } => {
            let u = moded(
                &bases.subjects,
                "/api/subjects/company",
                take.mode("company"),
            );
            let resp = refuse(
                client
                    .post(&u)
                    .json(&json!({ "id": id, "label": label }))
                    .send()?,
                &format!("POST {u}"),
            )?;
            let out: BatchAnswer = resp
                .json()
                .with_context(|| format!("POST {u}: the outcome did not parse"))?;
            Ok(out.line(Some("company")))
        }
        Door::Policy { path, .. } => {
            let out = boss_policy::bootstrap::publish_policy_rules(
                &bases.policy,
                path,
                take.has("policy"),
                "tenant-seed",
                Some(SEED_USER),
            )?;
            Ok(out.summary())
        }
        Door::People { roster } => {
            seed_people(client, &bases.people, roster, take.has("employees"))
        }
        Door::Agents { rows } => {
            let u = moded(&bases.jobs, "/api/agents/batch", take.mode("agents"));
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
            let out = boss_jobs::bootstrap::publish_workflows(
                &bases.jobs,
                path,
                owning_team,
                true,
                take.has("workflows"),
                Some(SEED_USER),
            )?;
            Ok(out.summary())
        }
        Door::Credentials { tenant_id, rows } => {
            let u = url(&bases.jobs, "/api/credentials/batch");
            let resp = refuse(
                client
                    .post(&u)
                    .json(&json!({ "tenant_id": tenant_id, "credentials": rows }))
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
        Door::Sensors { tenant_id, rows } => {
            let u = url(&bases.jobs, "/api/sensors/batch");
            let resp = refuse(
                client
                    .post(&u)
                    .json(&json!({ "tenant_id": tenant_id, "sensors": rows }))
                    .send()?,
                &format!("POST {u}"),
            )?;
            let out: BatchAnswer = resp
                .json()
                .with_context(|| format!("POST {u}: the outcome did not parse"))?;
            // No door overwrites a sensor, and the line says so rather
            // than naming a flag that does not exist.
            Ok(out.line(None))
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
/// - a version of the tenant's OWN above the file's → left alone, said
///   so (never walked back; an operator's live edit survives);
/// - the file's version present among the tenant's own, at any status
///   → no-op, said so — and if the stored content differs from the
///   file's, the field is named with "bump `version`", because a
///   silent edit under an unchanged version would otherwise read as
///   `present` forever;
/// - otherwise → draft + publish, at the file's version, `source`
///   riding the draft; the promoted row is checked to be the one just
///   drafted (publish promotes the NEWEST draft, and an operator's
///   armed draft would be it otherwise).
///
/// A name another source holds LIVE (active or draft) is refused by
/// the door (400, naming both owners) — read here from the versions
/// first, so the refusal names the owner before a draft is even
/// attempted. A NAME WHOSE OTHER-SOURCE ROWS ARE ALL RETIRED IS FREE
/// (backlog 70bc5725, 2026-09-18): on the playground's fresh database
/// the historical migrations insert the demo tenant's thirty-one reactors
/// as product rows, the boot seed retires them, and this function
/// refused every one — so the tenant's `seeds/rules.toml` could land
/// on no instance at all. The takeover is the door's ordinary draft:
/// it lands at `max(declared, MAX + 1)`, which is ABOVE the retired
/// history when the file declares the version the migration did, so
/// the landing version is computed the way the door computes it and
/// the line names the version the row actually took and the source
/// it took the name from. The next publish then reads the tenant's
/// own row as ahead of its file, and leaves it alone.
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
    let (mine, theirs): (Vec<&Value>, Vec<&Value>) = versions
        .iter()
        .partition(|v| v["source"].as_str() == Some(source));
    if let Some(other) = theirs.iter().find(|v| v["status"] != json!("retired")) {
        bail!(
            "{name}: owned by {}; a rule from {source} cannot supersede it — declare it under a \
             name of your own",
            source_label(other["source"].as_str())
        );
    }
    // The other source's rows are all retired: history under the name,
    // which the tenant's own file is not compared against.
    let taken_from = theirs.first().map(|v| source_label(v["source"].as_str()));
    let max = mine.iter().filter_map(|v| v["version"].as_u64()).max();
    if let Some(max) = max.filter(|m| *m > want) {
        return Ok(format!(
            "{name} v{want}: registry ahead at v{max}, left alone"
        ));
    }
    if let Some(stored) = mine.iter().find(|v| v["version"] == json!(want)) {
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
    // Where the door lands a draft: `max(declared, MAX + 1)` over EVERY
    // row of the name. With no other source's history that is the
    // file's version (the checks above returned otherwise); over a
    // retired history it is the first version above it.
    let history_max = versions.iter().filter_map(|v| v["version"].as_u64()).max();
    let expect = history_max.map_or(want, |m| want.max(m + 1));
    if drafted["version"] != json!(expect) {
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
    if promoted["version"] != json!(expect) || promoted["status"] != json!("active") {
        bail!(
            "{name}: publish promoted v{} ({}) rather than the v{expect} just drafted — an armed \
             draft from another author?",
            promoted["version"],
            promoted["status"]
        );
    }
    Ok(match (max, taken_from) {
        (Some(prev), _) => format!("{name} v{want}: published, superseding v{prev}"),
        (None, Some(owner)) if expect != want => format!(
            "{name} v{want}: published as v{expect}, taking over the name from {owner} (its \
             rows are retired; the file's version was already in the history)"
        ),
        (None, Some(owner)) => {
            format!(
                "{name} v{want}: published, taking over the name from {owner} (its rows are retired)"
            )
        }
        (None, None) => format!("{name} v{want}: published"),
    })
}

/// Run the plan against `bases`, one line per step through `out` as
/// each lands; `take` names the registries whose live rows this run
/// may overwrite (design e187198f; empty = none). Refuses a plan with
/// any Refused step BEFORE the first write. Blocking HTTP — call from
/// `spawn_blocking`.
pub fn publish(plan: &Plan, bases: &Bases, take: &Take, out: &mut dyn FnMut(String)) -> Result<()> {
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
                    send(&client, bases, door, take)
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
        // The credential the sensor names (backlog ee368d0c): declared
        // by the instance, before the sensor that names it.
        put(
            &dir,
            "seeds/credentials.toml",
            "[[credential]]\nid = \"stripe-restricted-read\"\nkind = \"stripe-restricted-key\"\n\
             issuer = \"stripe (the dashboard, restricted key)\"\n\
             principal = \"the company's Stripe account\"\nscopes = [\"charges: read\"]\n\
             storage_location = \"k8s Secret boss/boss-credential-broker-root key stripe-restricted-read\"\n\
             consumers = [{ kind = \"env\", location = \"dispatcher env BOSS_BROKER_STRIPE_KEY\" }]\n",
        );
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
                "seeds/credentials.toml",
                "seeds/sensors.toml",
                "seeds/posting_rules.toml",
                "seeds/fact_projection_rules.toml",
                "seeds/rules.toml",
            ],
            "classes → chart of accounts → locations → calendars → company → policy → people → agents → workflows → credentials → sensors → posting rules → projections → rules LAST"
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
        match &step(&p, "seeds/credentials.toml").action {
            Action::Write(Door::Credentials { tenant_id, rows }) => {
                assert_eq!(tenant_id, "acme");
                assert_eq!(rows.len(), 1);
                assert_eq!(rows[0].id, "stripe-restricted-read");
                assert_eq!(rows[0].rotation_policy, "on-demand");
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
        let err = publish(&p, &bases, &Take::default(), &mut |l| lines.push(l)).unwrap_err();
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
        /// What "exists" server-side: policy rules by id (the row the
        /// GET answers, so the bootstrap's comparison sees it),
        /// employee ids, published workflow kinds (kind -> the live
        /// spec the GET answers), design Job ids.
        policy: BTreeMap<String, Value>,
        people: BTreeMap<String, Value>,
        workflows: BTreeMap<String, Value>,
        jobs: usize,
        /// Published sensors, id -> the declared row (insert-if-absent,
        /// like the door; a held row that differs is named).
        sensors: BTreeMap<String, Value>,
        /// Published business calendars, code -> the row (insert-if-
        /// absent; `?mode=take` replaces).
        calendars: BTreeMap<String, Value>,
        /// The company subject's label, once minted.
        company: Option<String>,
        /// What the stub's "publish" lands as a kind's live row: the
        /// fixture file's own specs (`stub_for`), so a second publish
        /// reads the row as the file declares it — the real registry
        /// lands the spec through the design Job's publish step, which
        /// this stub does not walk.
        file_specs: BTreeMap<String, Value>,
        /// Published classes, "kind/code" -> the row (insert-if-absent;
        /// `PUT /api/classes/{kind}/{code}` edits one).
        classes: BTreeMap<String, Value>,
        /// Declared credential ids (insert-if-absent, like the door).
        credentials: BTreeSet<String>,
        /// Published location ids (insert-if-absent, like the door).
        locations: BTreeSet<String>,
        /// Registered agents: id -> display_name (insert-if-absent by
        /// default, `?mode=take` overwrites; a test's "migration"
        /// pre-registers agent-claude under the platform's own name,
        /// as the real schema does).
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
                + self.credentials.len()
                + self.locations.len()
                + self.agents.len()
                + self.posting_rules.len()
                + self.projections.len()
                + self.accounts.len()
                + self.rules.len()
                + self.calendars.len()
                + self.classes.len()
                + usize::from(self.company.is_some())
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
    /// its `source`, refused when another source holds a LIVE row of
    /// the name (a retired name is free, backlog 70bc5725); publish
    /// promotes the newest draft and retires the incumbent.
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
                if let Some(other) = st.rules.iter().find(|r| {
                    r["name"] == name && r["status"] != "retired" && r["source"] != source
                }) {
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

    /// The declared fields of `declared` that `held` reads differently
    /// — the comparison every insert-if-absent stub route answers a
    /// kept row with, over the keys the declaration carries.
    fn differs(held: &Value, declared: &Value) -> Vec<String> {
        declared
            .as_object()
            .map(|d| {
                d.iter()
                    .filter(|(k, v)| *k != "id" && *k != "code" && held.get(*k) != Some(v))
                    .map(|(k, _)| k.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The changes from `held` to `declared` on `fields`, the take's
    /// answer.
    fn changes(held: &Value, declared: &Value, fields: &[String]) -> Vec<Value> {
        fields
            .iter()
            .map(|f| {
                json!({"field": f, "from": held.get(f).cloned().unwrap_or(Value::Null),
                            "to": declared.get(f).cloned().unwrap_or(Value::Null)})
            })
            .collect()
    }

    /// One insert-if-absent / take batch over a `key -> row` map, the
    /// real doors' answer shape: `{received, inserted, kept, updated,
    /// unchanged}`.
    fn batch_into(
        store: &mut BTreeMap<String, Value>,
        rows: &[Value],
        key_of: impl Fn(&Value) -> String,
        take: bool,
    ) -> Value {
        let (mut inserted, mut unchanged) = (0usize, 0usize);
        let (mut kept, mut updated) = (Vec::new(), Vec::new());
        for r in rows {
            let key = key_of(r);
            match store.get(&key).cloned() {
                None => {
                    store.insert(key, r.clone());
                    inserted += 1;
                }
                Some(held) => {
                    let d = differs(&held, r);
                    if d.is_empty() {
                        unchanged += 1;
                    } else if take {
                        updated.push(json!({"id": key, "changes": changes(&held, r, &d)}));
                        store.insert(key, r.clone());
                    } else {
                        kept.push(json!({"id": key, "differs": d}));
                    }
                }
            }
        }
        json!({"received": rows.len(), "inserted": inserted, "kept": kept,
               "updated": updated, "unchanged": unchanged})
    }

    fn route(st: &mut Stub, method: &str, path: &str, body: &str) -> (u16, String) {
        // The mode rides the query string; the path is matched bare.
        let (path, take) = match path.split_once('?') {
            Some((p, q)) => (p, q == "mode=take"),
            None => (path, false),
        };
        let seg = |i: usize| path.split('/').nth(i).unwrap_or("").to_string();
        match (method, path) {
            ("POST", "/api/classes/batch") => {
                let rows = serde_json::from_str::<Vec<Value>>(body).unwrap_or_default();
                let key = |r: &Value| {
                    format!(
                        "{}/{}",
                        r["subject_kind"].as_str().unwrap_or(""),
                        r["code"].as_str().unwrap_or("")
                    )
                };
                // The real batch has no take: insert-if-absent, kept
                // rows named; the edit door below is the take.
                let mut out = batch_into(&mut st.classes, &rows, key, false);
                out.as_object_mut().unwrap().remove("updated");
                (200, out.to_string())
            }
            ("GET", p) if p.starts_with("/api/classes/") => {
                let key = format!("{}/{}", seg(3), seg(4));
                match st.classes.get(&key) {
                    Some(v) => (200, v.to_string()),
                    None => (404, "no such class".into()),
                }
            }
            ("PUT", p) if p.starts_with("/api/classes/") => {
                let key = format!("{}/{}", seg(3), seg(4));
                let row: Value = serde_json::from_str(body).unwrap_or(Value::Null);
                match st.classes.get_mut(&key) {
                    Some(held) => {
                        *held = row.clone();
                        (200, row.to_string())
                    }
                    None => (404, "no such class".into()),
                }
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
                // Insert by id; a held row that differs on the declared
                // field (here: display_name only) is kept and named by
                // default, updated with the change named from → to
                // under `?mode=take`, as the real door answers.
                let rows = serde_json::from_str::<Vec<Value>>(body).unwrap_or_default();
                let mut inserted = 0usize;
                let mut unchanged = 0usize;
                let mut updated = Vec::new();
                let mut kept = Vec::new();
                for r in &rows {
                    let id = r["id"].as_str().unwrap_or("").to_string();
                    let name = r["display_name"].as_str().unwrap_or("").to_string();
                    match st.agents.get(&id).cloned() {
                        Some(have) if have == name => unchanged += 1,
                        Some(have) if take => {
                            updated.push(json!({
                                "id": id,
                                "changes": [{"field": "display_name", "from": have, "to": name}]
                            }));
                            st.agents.insert(id, name);
                        }
                        Some(_) => kept.push(json!({"id": id, "differs": ["display_name"]})),
                        None => {
                            st.agents.insert(id, name);
                            inserted += 1;
                        }
                    }
                }
                (
                    200,
                    json!({
                        "received": rows.len(), "inserted": inserted,
                        "updated": updated, "kept": kept, "unchanged": unchanged
                    })
                    .to_string(),
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
            ("POST", "/api/calendar/business-calendars/batch") => {
                let rows = serde_json::from_str::<Vec<Value>>(body).unwrap_or_default();
                let key = |r: &Value| r["code"].as_str().unwrap_or("").to_string();
                let out = batch_into(&mut st.calendars, &rows, key, take);
                (200, out.to_string())
            }
            ("POST", "/api/subjects/company") => {
                // The mint, for one row: insert-if-absent by id, the
                // label kept and named by default, taken under take.
                let v: Value = serde_json::from_str(body).unwrap_or(Value::Null);
                let label = v["label"].as_str().unwrap_or("").to_string();
                match st.company.clone() {
                    None => {
                        st.company = Some(label);
                        (201, json!({"received": 1, "inserted": 1, "kept": [], "updated": [], "unchanged": 0}).to_string())
                    }
                    Some(have) if have == label => (
                        200,
                        json!({"received": 1, "inserted": 0, "kept": [], "updated": [], "unchanged": 1}).to_string(),
                    ),
                    Some(have) if take => {
                        st.company = Some(label.clone());
                        (
                            200,
                            json!({"received": 1, "inserted": 0, "kept": [],
                                   "updated": [{"id": v["id"], "changes": [{"field": "label", "from": have, "to": label}]}],
                                   "unchanged": 0})
                            .to_string(),
                        )
                    }
                    Some(_) => (
                        200,
                        json!({"received": 1, "inserted": 0, "kept": [{"id": v["id"], "differs": ["label"]}],
                               "updated": [], "unchanged": 0})
                        .to_string(),
                    ),
                }
            }
            ("GET", p) if p.starts_with("/api/policy/rules/") => match st.policy.get(&seg(4)) {
                Some(rule) => (200, rule.to_string()),
                None => (404, String::new()),
            },
            ("POST", "/api/policy/rules") => {
                let rule = serde_json::from_str::<Value>(body)
                    .map(|v| v["rule"].clone())
                    .unwrap_or(Value::Null);
                let id = rule["id"].as_str().unwrap_or_default().to_string();
                st.policy.insert(id, rule);
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
            ("GET", p) if p.starts_with("/api/workflows/") => match st.workflows.get(&seg(3)) {
                Some(live) => (200, live.to_string()),
                None => (404, String::new()),
            },
            ("POST", "/api/jobs") => {
                // A design Job for the kind: the stub's "publish" lands
                // the live row from the fixture's own file (its facets
                // are what the bootstrap compares against next time),
                // marked operator-published by its authoring_job_id.
                let v: Value = serde_json::from_str(body).unwrap_or(Value::Null);
                let kind = v["subject"]["id"].as_str().unwrap_or("").to_string();
                st.jobs += 1;
                let mut live = st.file_specs.get(&kind).cloned().unwrap_or_else(
                    || json!({"kind": kind, "label": "", "category": "", "steps": []}),
                );
                live["authoring_job_id"] = json!(format!("job-{}", st.jobs));
                st.workflows.insert(kind, live);
                (200, json!({"id": format!("job-{}", st.jobs)}).to_string())
            }
            ("POST", "/api/credentials/batch") => {
                let v: Value = serde_json::from_str(body).unwrap_or(Value::Null);
                assert_eq!(v["tenant_id"], "acme", "the batch names the tenant: {body}");
                let rows = v["credentials"].as_array().cloned().unwrap_or_default();
                for r in &rows {
                    assert!(
                        r.get("value").is_none() && r.get("token").is_none(),
                        "a declaration carries locations, never a value: {r}"
                    );
                }
                let inserted = rows
                    .iter()
                    .filter_map(|r| r["id"].as_str().map(str::to_string))
                    .filter(|id| st.credentials.insert(id.clone()))
                    .count();
                (
                    200,
                    json!({"received": rows.len(), "inserted": inserted}).to_string(),
                )
            }
            ("POST", "/api/sensors/batch") => {
                let v: Value = serde_json::from_str(body).unwrap_or(Value::Null);
                assert_eq!(v["tenant_id"], "acme", "the batch names the tenant: {body}");
                let rows = v["sensors"].as_array().cloned().unwrap_or_default();
                let key = |r: &Value| r["id"].as_str().unwrap_or("").to_string();
                // No take at this door: insert-if-absent, kept named.
                let mut out = batch_into(&mut st.sensors, &rows, key, false);
                out.as_object_mut().unwrap().remove("updated");
                (200, out.to_string())
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

    /// A stub whose "publish" of a workflow kind lands the fixture's
    /// own spec as the live row (see `Stub::file_specs`).
    fn stub_for(dir: &Path) -> Arc<Mutex<Stub>> {
        let specs = boss_jobs::seed_loader::load_workflows_with_owning_team(
            dir.join("seeds/workflows.toml"),
            "acme",
        )
        .unwrap();
        let mut st = Stub::default();
        for s in specs {
            st.file_specs
                .insert(s.kind.clone(), serde_json::to_value(&s).unwrap());
        }
        Arc::new(Mutex::new(st))
    }

    async fn run_publish(p: Plan, base: String) -> Result<Vec<String>> {
        run_publish_taking(p, base, Take::default()).await
    }

    async fn run_publish_taking(p: Plan, base: String, take: Take) -> Result<Vec<String>> {
        tokio::task::spawn_blocking(move || {
            let bases = Bases::resolve(Some(&base));
            let mut lines = Vec::new();
            publish(&p, &bases, &take, &mut |l| lines.push(l))?;
            Ok(lines)
        })
        .await
        .unwrap()
    }

    fn line_of<'a>(lines: &'a [String], path: &str) -> &'a str {
        lines
            .iter()
            .find(|l| l.contains(path))
            .unwrap_or_else(|| panic!("{path} has a line:\n{}", lines.join("\n")))
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
            hit("POST", "/api/credentials/batch"),
            1,
            "one batch for the credentials file"
        );
        assert_eq!(
            st.credentials.iter().collect::<Vec<_>>(),
            ["stripe-restricted-read"]
        );
        assert_eq!(
            hit("POST", "/api/sensors/batch"),
            1,
            "one batch for the sensors file"
        );
        assert_eq!(
            st.sensors.keys().collect::<Vec<_>>(),
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
            pos("POST", "/api/credentials/batch") < pos("POST", "/api/sensors/batch"),
            "a sensor names a credential by id, so the credentials go first (backlog ee368d0c)"
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
            people_line.contains("2 posted, 1/1 linked"),
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
        let st = stub_for(&dir);
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
        assert_eq!(
            second
                .iter()
                .filter(|(m, p, _, _)| m == "PUT" && p.starts_with("/api/people/"))
                .count(),
            0,
            "every row and every manager edge is already as declared, so nothing is PUT \
             (backlog 09887242): {second:?}"
        );
        let people_line = lines
            .iter()
            .find(|l| l.contains("seeds/employees.json"))
            .unwrap();
        assert!(
            people_line.contains("0 posted, 2 already as declared, 1/1 linked")
                && !people_line.contains("updated 1"),
            "{people_line}"
        );
        let agents_line = lines
            .iter()
            .find(|l| l.contains("seeds/agents.toml"))
            .unwrap();
        assert!(
            agents_line.contains("received 1, inserted 0, 1 already as declared"),
            "{agents_line}"
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

    /// A NAME THE PRODUCT RETIRED IS FREE FOR THE TENANT (backlog
    /// 70bc5725, 2026-09-18) — the playground's shape: the historical
    /// migrations insert the demo tenant's thirty-one reactors as product
    /// rows, the boot seed retires them, and the tenant's file declares
    /// the same versions the migrations did. The publish read every
    /// row as the owner's and refused all thirty-one ("owned by
    /// product"); had it read only the live rows it would still have
    /// answered `present (retired)` off the product's row and landed
    /// nothing. Ownership is the live rows; the file's version is
    /// compared against the tenant's OWN rows; the draft lands where
    /// the door puts it (above the retired history) and the line says
    /// so. A second publish is a no-op that says the registry is
    /// ahead, and a product row still LIVE is still refused.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_rule_name_the_product_retired_is_taken_over_by_the_tenant() {
        let dir = real_shape("rule-takeover");
        let st = Arc::new(Mutex::new(Stub::default()));
        // The migration's row, retired by the seed: no file names it.
        st.lock().unwrap().rules.push(json!({
            "name": "complete-site-live-on-converge-closed", "version": 2, "status": "retired",
            "on_event": "jobs.job.closed", "when": "kind = \"maintenance-cluster-converge\"",
            "do": [{"handler": "messages.notify", "args": {}}], "delay": null,
            "created_at": "2026-09-10T00:00:00Z"
        }));
        let base = spawn_stub(st.clone()).await;

        let lines = run_publish(plan(&dir).unwrap(), base.clone())
            .await
            .unwrap();
        {
            let st = st.lock().unwrap();
            let live = st
                .active_rule("complete-site-live-on-converge-closed")
                .expect("the tenant's rule is live");
            assert_eq!(live["version"], 3, "above the product's retired v2");
            assert_eq!(live["source"], "tenant:acme");
            let history: Vec<(u64, &str, Option<&str>)> = st
                .rules
                .iter()
                .filter(|r| r["name"] == "complete-site-live-on-converge-closed")
                .map(|r| {
                    (
                        r["version"].as_u64().unwrap(),
                        r["status"].as_str().unwrap(),
                        r["source"].as_str(),
                    )
                })
                .collect();
            assert_eq!(
                history,
                [(2, "retired", None), (3, "active", Some("tenant:acme"))],
                "both sources in the history"
            );
        }
        let line = lines
            .iter()
            .find(|l| l.contains("seeds/rules.toml"))
            .unwrap();
        assert!(
            line.contains("v2: published as v3, taking over the name from product"),
            "{line}"
        );

        // Idempotent: the tenant's own row is now ahead of its file.
        let lines = run_publish(plan(&dir).unwrap(), base.clone())
            .await
            .unwrap();
        let line = lines
            .iter()
            .find(|l| l.contains("seeds/rules.toml"))
            .unwrap();
        assert!(
            line.contains("v2: registry ahead at v3, left alone"),
            "{line}"
        );
        assert_eq!(
            st.lock()
                .unwrap()
                .rules
                .iter()
                .filter(|r| r["name"] == "complete-site-live-on-converge-closed")
                .count(),
            2,
            "the second publish wrote nothing"
        );

        // A product row that is still LIVE is still the product's.
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
        assert!(
            msg.contains("owned by product") && msg.contains("tenant:acme"),
            "{msg}"
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

    /// A REGISTERED AGENT THAT DIFFERS IS KEPT BY DEFAULT AND NAMED;
    /// `--take agents` APPLIES THE DECLARATION AND NAMES THE CHANGE
    /// (design e187198f; the door f56155f0 opened, the rule 09887242
    /// made a decision). The platform's migration registered
    /// `agent-claude` under its own display name; the tenant declares
    /// the same id under another. By default the instance's row is
    /// kept and the line reads `kept: agent-claude differs on
    /// display_name (the instance is the truth; --take agents
    /// overwrites)` — the batch is sent WITHOUT `?mode=take`; under
    /// `--take agents` the batch goes with `?mode=take`, the field is
    /// applied, the line reads `updated 1: agent-claude (display_name
    /// <from> → <to>)`, and the next publish says `already as declared`.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_registered_agent_that_differs_is_kept_by_default_and_taken_only_by_decision() {
        let dir = real_shape("agent-updated");
        let p = plan(&dir).unwrap();
        let st = Arc::new(Mutex::new(Stub::default()));
        st.lock().unwrap().agents.insert(
            "agent-claude".into(),
            "Claude (Claude Code sessions on the dev pod)".into(),
        );
        let base = spawn_stub(st.clone()).await;
        let lines = run_publish(p.clone(), base.clone()).await.unwrap();
        let agents_line = line_of(&lines, "seeds/agents.toml");
        assert!(
            agents_line.contains("POST /api/agents/batch")
                && agents_line.contains(
                    "received 1, inserted 0; kept: agent-claude differs on display_name (the \
                     instance is the truth; --take agents overwrites)"
                ),
            "{agents_line}"
        );
        {
            let st = st.lock().unwrap();
            assert_eq!(
                st.agents["agent-claude"], "Claude (Claude Code sessions on the dev pod)",
                "the instance's row is kept"
            );
            assert!(
                st.log
                    .iter()
                    .any(|(m, p, _, _)| m == "POST" && p == "/api/agents/batch"),
                "the batch went without a mode: {:?}",
                st.log.iter().map(|l| &l.1).collect::<Vec<_>>()
            );
        }

        let lines = run_publish_taking(p.clone(), base.clone(), Take::named(&["agents"]))
            .await
            .unwrap();
        let agents_line = line_of(&lines, "seeds/agents.toml");
        assert!(
            agents_line.contains(
                "received 1, inserted 0, updated 1: agent-claude (display_name Claude \
                 (Claude Code sessions on the dev pod) → Claude (engineering))"
            ) && !agents_line.contains("kept:"),
            "{agents_line}"
        );
        {
            let st = st.lock().unwrap();
            assert_eq!(
                st.agents["agent-claude"], "Claude (engineering)",
                "the declaration wins under take"
            );
            assert!(
                st.log
                    .iter()
                    .any(|(m, p, _, _)| m == "POST" && p == "/api/agents/batch?mode=take"),
                "the take rode the query string: {:?}",
                st.log.iter().map(|l| &l.1).collect::<Vec<_>>()
            );
        }
        let lines = run_publish(p, base).await.unwrap();
        let agents_line = line_of(&lines, "seeds/agents.toml");
        assert!(
            agents_line.contains("received 1, inserted 0, 1 already as declared")
                && !agents_line.contains("updated 1")
                && !agents_line.contains("kept:"),
            "{agents_line}"
        );
    }

    /// `--take` NAMES ONLY A REGISTRY A DOOR CAN OVERWRITE. A typo or a
    /// registry with no overwrite (sensors) is refused naming the
    /// list, so a mis-spelt take never reads as "nothing taken".
    #[test]
    fn take_parses_the_takeable_registries_and_refuses_the_rest() {
        let t = Take::parse(Some("agents, employees")).unwrap();
        assert!(t.has("agents") && t.has("employees") && !t.has("classes"));
        assert_eq!(t.mode("agents"), PublishMode::Take);
        assert_eq!(t.mode("classes"), PublishMode::InsertIfAbsent);
        assert!(Take::parse(None).unwrap().is_empty());
        assert!(Take::parse(Some("")).unwrap().is_empty());
        for bad in ["sensors", "agent", "everything"] {
            let err = Take::parse(Some(bad)).unwrap_err().to_string();
            assert!(
                err.contains(bad) && err.contains("classes, calendars"),
                "{err}"
            );
        }
        assert!(
            Take::default()
                .describe()
                .contains("every door is insert-if-absent")
        );
        assert!(t.describe().contains("take: agents, employees"));
    }

    /// AN EMPLOYEE ALREADY THERE IS KEPT BY DEFAULT WITH THE DIFFERING
    /// DECLARED FIELDS NAMED; `--take employees` APPLIES THEM AND KEEPS
    /// THE REST (design e187198f over backlog 09887242). Measured
    /// 2026-09-17 on prod: emp-david read `loc-hq` while the tenant's
    /// employees.json said `loc-algedonic-hq`, and the 409 on the
    /// re-POST was counted as "already there" with nothing compared —
    /// the silence 09887242 fixed by applying the declaration on every
    /// publish, which then reverted every operator edit at the next
    /// boot. Now the 409's GET is compared key by key against the
    /// declaration: by default the row is kept and the line names the
    /// fields; under take a declared key that differs is PUT (the
    /// door's own `people.employee.updated`), a column the tenant does
    /// not declare (here a salary set out of band) rides the PUT
    /// unchanged, the line names the change, and the next publish
    /// PUTs nothing.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_employee_that_differs_is_kept_by_default_and_taken_on_declared_fields_only() {
        let dir = real_shape("employee-updated");
        // The tenant declares the founder at its own site and does not
        // declare a salary at all.
        put(
            &dir,
            "seeds/employees.json",
            r#"[{"id": "emp-david", "name": "David Auld", "email": "david@acme.example",
  "role": "founder", "department": "engineering", "skill_level": null,
  "hire_date": "2026-09-16", "location": "loc-acme-hq", "manager_id": null,
  "employment_type": "full-time", "status": "active", "skills": [], "certifications": []}]"#,
        );
        let p = plan(&dir).unwrap();
        let st = Arc::new(Mutex::new(Stub::default()));
        // What prod held: the first publish's row, at the platform's
        // loc-hq, plus a salary an operator set through the people API.
        st.lock().unwrap().people.insert(
            "emp-david".into(),
            json!({"id": "emp-david", "name": "David Auld", "email": "david@acme.example",
                "role": "founder", "department": "engineering", "skill_level": null,
                "hire_date": "2026-09-16", "location": "loc-hq", "manager_id": null,
                "employment_type": "full-time", "status": "active", "skills": [],
                "certifications": [], "annual_salary_cents": 12_000_000}),
        );
        let base = spawn_stub(st.clone()).await;
        // By default: kept, named, no PUT.
        let lines = run_publish(p.clone(), base.clone()).await.unwrap();
        let people_line = line_of(&lines, "seeds/employees.json");
        assert!(
            people_line.contains(
                "0 posted, 0/0 linked; kept: emp-david differs on location (the instance is \
                 the truth; --take employees overwrites)"
            ) && !people_line.contains("updated"),
            "{people_line}"
        );
        {
            let st = st.lock().unwrap();
            assert_eq!(
                st.people["emp-david"]["location"], "loc-hq",
                "the instance's row is kept"
            );
            assert!(
                !st.log.iter().any(|(m, _, _, _)| m == "PUT"),
                "nothing is PUT under the default: {:?}",
                st.log.iter().map(|l| (&l.0, &l.1)).collect::<Vec<_>>()
            );
        }
        // Under --take employees: the declared field is applied.
        let lines = run_publish_taking(p.clone(), base.clone(), Take::named(&["employees"]))
            .await
            .unwrap();
        let people_line = line_of(&lines, "seeds/employees.json");
        assert!(
            people_line.contains(
                "0 posted, updated 1: emp-david (location loc-hq → loc-acme-hq), 0/0 linked"
            ) && !people_line.contains("kept:"),
            "{people_line}"
        );
        {
            let st = st.lock().unwrap();
            let row = &st.people["emp-david"];
            assert_eq!(
                row["location"], "loc-acme-hq",
                "the declared field is applied"
            );
            assert_eq!(
                row["annual_salary_cents"], 12_000_000,
                "the column the tenant did not declare is kept"
            );
            assert_eq!(
                st.log
                    .iter()
                    .filter(|(m, p, _, _)| m == "PUT" && p == "/api/people/emp-david")
                    .count(),
                1,
                "one PUT, through the people door's own update"
            );
        }
        // Idempotent: the same declaration again compares equal and
        // PUTs nothing.
        let first_len = st.lock().unwrap().log.len();
        let lines = run_publish(p, base).await.unwrap();
        let people_line = lines
            .iter()
            .find(|l| l.contains("seeds/employees.json"))
            .unwrap();
        assert!(
            people_line.contains("0 posted, 1 already as declared, 0/0 linked")
                && !people_line.contains("updated 1"),
            "{people_line}"
        );
        let st = st.lock().unwrap();
        assert!(
            !st.log[first_len..].iter().any(|(m, _, _, _)| m == "PUT"),
            "the second publish updated nothing: {:?}",
            &st.log[first_len..]
        );
    }

    /// EVERY REGISTRY'S LINE NAMES ITS KEPT-BUT-DIFFERING ROWS, IN ONE
    /// SHAPE (design e187198f). The instance holds an edited row of
    /// every registry that used to say nothing — a class, a calendar,
    /// the company label, a policy rule, a sensor, a workflow — and the
    /// file's row differs. A default publish writes none of them and
    /// every line reads `kept: <id> differs on <fields> (the instance
    /// is the truth; --take <registry> overwrites)`, or, for the one
    /// registry no door overwrites, says so.
    #[tokio::test(flavor = "multi_thread")]
    async fn every_registry_names_its_kept_but_differing_rows_in_one_shape() {
        let dir = real_shape("kept-lines");
        let p = plan(&dir).unwrap();
        let st = stub_for(&dir);
        let base = spawn_stub(st.clone()).await;
        // Land the file once, then edit the instance behind it the way
        // an operator would: a renamed class, a longer closed set, a
        // new company label, a narrower policy scope, a slower sensor,
        // a re-described workflow with a step added in /it/registry.
        run_publish(p.clone(), base.clone()).await.unwrap();
        {
            let mut st = st.lock().unwrap();
            st.classes.get_mut("employee/founder").unwrap()["display_name"] =
                json!("Founder & CEO");
            st.calendars.get_mut("acme-founder").unwrap()["closed"] =
                json!(["2026-12-25", "2026-12-26"]);
            st.company = Some("Acme Holdings, LLC".into());
            st.policy.get_mut("founder:job:read").unwrap()["scope"] = json!("team");
            st.sensors.get_mut("stripe-sponsorships").unwrap()["every_minutes"] = json!(60);
            let live = st.workflows.get_mut("receive-a-sponsorship").unwrap();
            live["description"] = json!("edited in the registry");
            live["steps"].as_array_mut().unwrap().push(json!({
                "title": "thanked", "kind": "task", "ready_when": "steps.sponsored.done",
                "title_template": "Thank the sponsor", "fields": []
            }));
        }
        let before = st.lock().unwrap().writes();

        let lines = run_publish(p.clone(), base.clone()).await.unwrap();
        assert_eq!(
            st.lock().unwrap().writes(),
            before,
            "a default publish writes nothing over an edited instance"
        );
        for (path, want) in [
            (
                "seeds/classes.json",
                "received 2, inserted 0, 1 already as declared; kept: employee/founder differs on \
                 display_name (the instance is the truth; --take classes overwrites)",
            ),
            (
                "seeds/business_calendars.json",
                "received 1, inserted 0; kept: acme-founder differs on closed (the instance is \
                 the truth; --take calendars overwrites)",
            ),
            (
                "tenant.toml",
                "received 1, inserted 0; kept: acme differs on label (the instance is the truth; \
                 --take company overwrites)",
            ),
            (
                "seeds/policy_rules.toml",
                "0 posted, 3 already as declared; kept: founder:job:read differs on scope (the \
                 instance is the truth; --take policy overwrites)",
            ),
            (
                "seeds/sensors.toml",
                "received 1, inserted 0; kept: stripe-sponsorships differs on every_minutes (the \
                 instance is the truth; no door overwrites this registry)",
            ),
            (
                "seeds/workflows.toml",
                "0 published; kept: receive-a-sponsorship differs on description, steps.count, \
                 steps.titles (the instance is the truth; --take workflows overwrites)",
            ),
        ] {
            let line = line_of(&lines, path);
            assert!(line.contains(want), "{path}:\n  got  {line}\n  want {want}");
        }
        // The class the instance did not edit is not a finding.
        assert!(
            !line_of(&lines, "seeds/classes.json").contains("engineering"),
            "{}",
            line_of(&lines, "seeds/classes.json")
        );

        // `--take` on every takeable registry: each overwritten row is
        // printed field by field, the instance now reads the file, and
        // the next default publish names nothing.
        let lines = run_publish_taking(
            p.clone(),
            base.clone(),
            Take::named(&["classes", "calendars", "company", "policy", "workflows"]),
        )
        .await
        .unwrap();
        for (path, want) in [
            (
                "seeds/classes.json",
                "received 2, inserted 0, updated 1: employee/founder (display_name Founder & CEO \
                 → Founder), 1 already as declared",
            ),
            (
                "seeds/business_calendars.json",
                "received 1, inserted 0, updated 1: acme-founder (closed \
                 [\"2026-12-25\",\"2026-12-26\"] → [\"2026-12-25\"])",
            ),
            (
                "tenant.toml",
                "received 1, inserted 0, updated 1: acme (label Acme Holdings, LLC → Acme, LLC)",
            ),
            (
                "seeds/policy_rules.toml",
                "0 posted, updated 1: founder:job:read (scope team → all), 3 already as declared",
            ),
            (
                "seeds/workflows.toml",
                "0 published, superseded 1: receive-a-sponsorship (description edited in the \
                 registry → , steps.count 4 → 3, steps.titles \
                 received,reconcile,sponsored,thanked → received,reconcile,sponsored)",
            ),
        ] {
            let line = line_of(&lines, path);
            assert!(
                line.contains(want) && !line.contains("kept:"),
                "{path}:\n  got  {line}\n  want {want}"
            );
        }
        {
            let st = st.lock().unwrap();
            assert_eq!(st.classes["employee/founder"]["display_name"], "Founder");
            assert_eq!(
                st.calendars["acme-founder"]["closed"],
                json!(["2026-12-25"])
            );
            assert_eq!(st.company.as_deref(), Some("Acme, LLC"));
            assert_eq!(st.policy["founder:job:read"]["scope"], "all");
            assert!(
                st.log
                    .iter()
                    .any(|(m, p, _, _)| m == "PUT" && p == "/api/classes/employee/founder"),
                "the class take went through the edit door"
            );
            assert!(
                st.log.iter().any(|(m, p, _, _)| m == "POST"
                    && p == "/api/calendar/business-calendars/batch?mode=take"),
                "the calendar take rode the query string"
            );
            assert!(
                st.log
                    .iter()
                    .any(|(m, p, _, _)| m == "POST" && p == "/api/subjects/company?mode=take"),
                "the company take rode the query string"
            );
        }
        let lines = run_publish(p, base).await.unwrap();
        for path in [
            "seeds/classes.json",
            "seeds/business_calendars.json",
            "tenant.toml",
            "seeds/policy_rules.toml",
            "seeds/workflows.toml",
        ] {
            let line = line_of(&lines, path);
            assert!(
                !line.contains("kept:") && !line.contains("updated"),
                "{path} after the take: {line}"
            );
        }
        assert!(
            line_of(&lines, "seeds/sensors.toml").contains("kept: stripe-sponsorships"),
            "the sensor was never taken: {}",
            line_of(&lines, "seeds/sensors.toml")
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
