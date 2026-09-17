//! `boss tenant init <name> [--into <dir>]` / `boss tenant check <dir>`
//! — a tenant is a directory of its own (backlog fcc1d57b).
//!
//! WHY THIS IS A VERB. Until 2026-09-16 a tenant had no home outside
//! the product tree: the only tenants were `examples/brewery` and
//! `examples/used-device-shop`, selected by `BOSS_TENANT_MANIFEST_TOML`
//! pointing INTO the product checkout. An adopter downloading BOSS had
//! nowhere natural to put their company except a patch to ours, and
//! Algedonic, LLC's own business could not live in `examples/` — the
//! public tier that mirrors to GitHub. Decided with David 2026-09-16:
//! a tenant is a directory OUTSIDE the product tree, in exactly the
//! shape of `examples/<tenant>/` (`tenant.toml` + `seeds/`); the
//! product's examples exercise the contract publicly; Algedonic's own
//! tenant is the private repo `david/algedonic-llc`, the first real
//! customer.
//!
//! THE CONTRACT IS MEASURED, NOT DESIGNED. [`CONTRACT`] is the list of
//! files the product actually READS from a tenant directory, found by
//! grepping every reader on 2026-09-16 (each entry's `read_by` names
//! it). One file the examples ship — `seeds/subject_kinds.toml` — has
//! NO reader today, and the table says so rather than implying one
//! (`seeds/locations.toml` was the other until its door landed on
//! 2026-09-17, backlog 1ec8312a). `docs/tenant-contract.md` carries
//! the same table between two markers; a test holds the two equal
//! (CLAUDE.md §9a), and `boss tenant contract` prints it for pasting.
//!
//! ONE LOADER, TWO DOORS. `check` validates each file with the type
//! or loader the product reads it with — `boss_jobs::seed_loader` for
//! workflows (with its viability lint), `boss_policy_client`'s grant
//! loader, the classes batch endpoint's `ClassInput`, the locations
//! batch endpoint's `LocationInput`, the ledger's `chart::AccountInput`
//! + `validate` for the chart of accounts (backlog 41af5195),
//! `boss_people`'s `Employee`, `boss_core`'s `BusinessCalendar` and
//! `TenantToml`, the dispatcher's own `[[rule]]` parser + publish-door
//! `validate` for `rules.toml` (backlog 458971ef). A second parser
//! would be a second contract that drifts. Where a file has no reader,
//! the parse is conservative and the table names that.
//!
//! WHAT A VERDICT CARRIES. Per file: OK / MISSING (required) / INVALID
//! (the loader's own error, never rephrased) / UNKNOWN (a file the
//! contract does not name — `seeds/agents.toml` in the real tenant was
//! the first such extension, until its seed landed on 2026-09-17,
//! backlog f56155f0). Exit 0 when nothing is MISSING or
//! INVALID. A file that parses to nothing because the loader reads a
//! different table name (`[[rule]]` where the loader reads `[[grants]]`)
//! is INVALID naming both: no evidence is not a pass.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use boss_core::tenant_manifest::TenantToml;

/// One file the product reads from a tenant directory. The order of
/// [`CONTRACT`] is the order `check` reports and the doc's table.
pub struct Entry {
    /// Canonical relative path first; accepted alternates after
    /// (`seeds/tenant.toml` is the N-1 spelling the examples keep
    /// because the deployment's `BOSS_TENANT_MANIFEST_TOML` points
    /// there — car 2 of fcc1d57b derives it from `BOSS_TENANT_DIR`).
    pub paths: &'static [&'static str],
    pub required: bool,
    /// Who reads it — the measured reader, named by crate or script.
    pub read_by: &'static str,
    /// The shape, in the reader's terms.
    pub shape: &'static str,
    /// How `check` judges it. `Ok(detail)` is one line of what was
    /// read; `Err(why)` is the loader's own error.
    parse: fn(&Path, &Ctx) -> Result<String, String>,
    /// What `init` writes, or `None` for a file that is documented but
    /// not scaffolded (a tenant engine's data, dead in a tenant with no
    /// engine).
    scaffold: Option<fn(&Scaffold) -> String>,
}

/// What the parsers need from the rest of the directory.
struct Ctx {
    /// `[meta] tenant_id` — the `owning_team` workflows are stamped with.
    tenant_id: String,
}

/// What the scaffold templates need.
pub struct Scaffold {
    pub name: String,
    pub display_name: String,
}

pub const TABLE_BEGIN: &str = "<!-- contract-table:begin -->";
pub const TABLE_END: &str = "<!-- contract-table:end -->";

/// The contract. Measured 2026-09-16 by grepping every reader of
/// `examples/<tenant>/seeds/*` across `crates/` and `infra/`.
pub const CONTRACT: &[Entry] = &[
    Entry {
        paths: &["tenant.toml", "seeds/tenant.toml"],
        required: true,
        read_by: "boss-gateway `/api/tenant/manifest` (+ inlined into index.html) via \
                  `boss_core::tenant_manifest::TenantToml`, path `BOSS_TENANT_MANIFEST_TOML`; \
                  boss-sim `TenantConfig` reads the same file with sim-only sections \
                  (`seed`, `start_date`, `[job_rates]`) for a tenant that has an engine",
        shape: "`[meta] tenant_id` (required by check: it is every workflow's `owning_team`), \
                `display_name`; `[modules] <module> = bool`; `[labels] <dotted.key> = str`",
        parse: parse_manifest,
        scaffold: Some(scaffold_manifest),
    },
    Entry {
        paths: &["seeds/workflows.toml"],
        required: true,
        read_by: "boss-jobs `seed_loader::load_workflows_with_owning_team` + the viability lint \
                  (the tenant prepare publishes each row); \
                  infra/lint/the-live-protocols-are-the-authored-protocols.sh; \
                  infra/gcp/publish-workflow.sh",
        shape: "`[[workflow]]` rows (kind, label, category, subject_kinds, description, \
                metadata) each with flat `[[workflow.step]]` rows whose `ready_when` \
                predicates imply the DAG; >= 1 trigger and >= 1 terminal per workflow",
        parse: parse_workflows,
        scaffold: Some(scaffold_workflows),
    },
    Entry {
        paths: &["seeds/policy_rules.toml"],
        required: false,
        read_by: "boss-policy-bootstrap / `boss_policy::bootstrap::publish_policy_rules` via \
                  `boss_policy_client::seed_loader::load_policy_rules` (the tenant prepare, \
                  first boot)",
        shape: "`[[grants]]` rows: `role` or `roles`, `resource` or `resources`, `action` or `actions`, \
                `scope` (all/self/team/territory/none/department:<name>); expanded to one \
                rule per role x resource x action",
        parse: parse_policy_rules,
        scaffold: Some(scaffold_policy_rules),
    },
    Entry {
        paths: &["seeds/classes.json", "seeds/classes.toml"],
        required: false,
        read_by: "POST /api/classes/batch, one boss-classes `http::ClassInput` per row — sent by \
                  the tenant prepare (brewery: classes.json; used-device-shop: classes.toml \
                  `[[class]]`) and infra/postgres/reset-to-baseline.sh",
        shape: "JSON array (or TOML `[[class]]` rows) of {subject_kind, code, display_name, \
                parent_code?, member_attribute?, metadata?, sort_order?}",
        parse: parse_classes,
        scaffold: Some(scaffold_classes),
    },
    Entry {
        paths: &["seeds/chart_of_accounts.toml"],
        required: false,
        read_by: "POST /api/ledger/accounts/batch, one boss-ledger `chart::AccountInput` per row \
                  (insert-if-absent by code) — sent by `boss tenant publish` AFTER the classes; a \
                  code the starter chart (40-ledger.sql, the OSS default) already holds is the SAME \
                  account, kept under its registered name, and the publish line names the field \
                  the declaration differs on — adopt the code or choose another (backlog 41af5195; \
                  design 18cf4272)",
        shape: "`[[account]]` rows: code, name, kind (asset|liability|equity|revenue|expense), \
                normal_balance (debit|credit), parent? (a code declared earlier in the file) — \
                the `gl_accounts` table's authorable columns; validated by \
                `boss_ledger::chart::validate`",
        parse: parse_chart_of_accounts,
        scaffold: Some(scaffold_chart_of_accounts),
    },
    Entry {
        paths: &["seeds/employees.json"],
        required: false,
        read_by: "POST /api/people, one `boss_people::Employee` per row (the brewery engine's \
                  prepare reads it at the FIXED path /opt/boss/examples/brewery/seeds/, \
                  not from the bundle; used-device-shop reads data/employees.json instead)",
        shape: "JSON array of Employee rows: id, name, email, role, department, hire_date, \
                location, manager_id, employment_type, status, skills[], certifications[], \
                annual_salary_cents; role/department/location are validated against the \
                registries at write time, not here",
        parse: parse_employees,
        scaffold: Some(scaffold_employees),
    },
    Entry {
        paths: &["seeds/operator_hires.toml"],
        required: false,
        read_by: "boss-brewery-engine prepare (`seed_brewery_operator_hires`): each `[[hire]]` \
                  POSTed to /api/people as a `boss_people::Employee`",
        shape: "`[[hire]]` rows in the Employee shape above",
        parse: parse_operator_hires,
        scaffold: None,
    },
    Entry {
        paths: &["seeds/business_calendars.json"],
        required: false,
        read_by: "POST /api/calendar/business-calendars/batch as \
                  `Vec<boss_core::calendar::BusinessCalendar>` (the brewery engine's prepare); \
                  the dispatcher's timing triggers and the sim resolve business days from it",
        shape: "JSON array of {code, name, weekend: [0..6 Mon=0], closed: [YYYY-MM-DD]}",
        parse: parse_business_calendars,
        scaffold: Some(scaffold_business_calendars),
    },
    Entry {
        paths: &["seeds/sensors.toml"],
        required: false,
        read_by: "POST /api/sensors/batch (boss-jobs, insert-if-absent by id) — sent by `boss tenant \
                  publish` as the tenant's declarations; the dispatcher's `sensor.poll` handler \
                  reads the registry every 5 minutes and polls each due sensor (design 14c9b2ad); \
                  a push-only source is never due — the gateway's site surface records one \
                  `www-visits` reading per page view through POST /api/sensors/{id}/readings \
                  (backlog 0b5c5081)",
        shape: "`[[sensor]]` rows: id, source (`stripe` for succeeded charges and `stripe-payouts` \
                for paid payouts, both polled on the same credential; `site` push-only), credential \
                (a `credentials` registry id; none on a push-only source), every_minutes (none on a \
                push-only source), opens (the workflow kind one reading opens), subject_kind, \
                enabled? — validated by `boss_jobs::sensors::load_sensors_toml`",
        parse: parse_sensors,
        scaffold: Some(scaffold_sensors),
    },
    Entry {
        paths: &["seeds/agents.toml"],
        required: false,
        read_by: "POST /api/agents/batch (boss-jobs, insert-if-absent by id and by alias) — sent by \
                  `boss tenant publish` BEFORE the Workflows (a step's audience may name an agent); \
                  a row the platform already registered is kept and the publish line names any \
                  field the declaration differs on; the jobs API's login door resolves each alias \
                  to the id (design 6fda05ae; backlog f56155f0)",
        shape: "`[[agent]]` rows: id (`agent-<slug>`), display_name, default_model (a rate-card \
                model, e.g. `opus-5[1m]`), aliases? (the logins that sign as it), \
                hourly_budget_usd_micros?, max_concurrent_runs? — the `agents` table's columns \
                and nothing else; validated by `boss_jobs::agents::load_agents_toml`",
        parse: parse_agents,
        scaffold: Some(scaffold_agents),
    },
    Entry {
        paths: &["seeds/posting_rules.toml"],
        required: false,
        read_by: "POST /api/ledger/posting-rules/batch (boss-ledger, insert-if-absent by fact_kind + \
                  version, source = tenant:<id>) — sent by `boss tenant publish` AFTER the \
                  Workflows; the posting path evaluates a fact by the newest registry rule for \
                  its kind and by the code rules otherwise (backlog a40541cb)",
        shape: "`[[posting_rule]]` rows: fact_kind, version? (1), basis (cash|accrual), lines = \
                [{account_code, side (debit|credit), amount_path (a JSON pointer into the fact \
                payload, integer cents), memo?}] — the debit pointers and the credit pointers must \
                be the same multiset (balanced for every fact); validated by \
                `boss_ledger::posting_rules::load_posting_rules_toml`",
        parse: parse_posting_rules,
        scaffold: Some(scaffold_posting_rules),
    },
    Entry {
        paths: &["seeds/fact_projection_rules.toml"],
        required: false,
        read_by: "POST /api/ledger/fact-projection-rules/batch (boss-ledger, insert-if-absent by \
                  event_kind + when) — sent by `boss tenant publish` after the posting rules; the \
                  ledger's facts rebuild projects every matching audit_log event into a \
                  financial_fact (backlog a40541cb)",
        shape: "`[[projection]]` rows: event_kind (an audit_log kind), when? (a table of \
                {\"/pointer\" = value}, every pointer equal for the rule to fire), fact_kind, \
                source_table, source_id_path, happened_on_path?, created_by_path? — the \
                `gl_fact_projection_rules` columns; validated by \
                `boss_ledger::posting_rules::load_projection_rules_toml`",
        parse: parse_projection_rules,
        scaffold: Some(scaffold_projection_rules),
    },
    Entry {
        paths: &["seeds/locations.toml"],
        required: false,
        read_by: "POST /api/locations/batch, one boss-locations `http::LocationInput` per row \
                  (insert-if-absent by id) — sent by `boss tenant publish` BEFORE the roster, \
                  because an `employees.json` `location` is a foreign key into the registry \
                  (backlog 1ec8312a; until 2026-09-17 nothing read this file)",
        shape: "`[[location]]` rows: id, name, kind, timezone (+ parent_id, latitude, \
                longitude, address, account_id, metadata) — the `locations` table's columns",
        parse: parse_locations,
        scaffold: Some(scaffold_locations),
    },
    Entry {
        paths: &["seeds/rules.toml"],
        required: false,
        read_by: "POST /api/dispatcher/rules/_validate, then POST /api/dispatcher/rules (a draft \
                  carrying `source = tenant:<tenant_id>`) + POST /api/dispatcher/rules/{name}/publish \
                  per rule (boss-dispatcher) — sent by `boss tenant publish` LAST, after the \
                  Workflows a rule reacts on; append-only: an unchanged version is a no-op \
                  (the line says `present`), a higher version supersedes, a live version ahead \
                  of the file is left alone, and a name another source owns is refused; the \
                  dispatcher's boot seed retires only product-sourced rules no file names, so a \
                  tenant's rule survives every converge (backlog 458971ef)",
        shape: "`[[rule]]` rows in the product rule file's own shape (infra/dispatcher/rules/*.toml): \
                name, why, version, on_event or schedule, when?, delay?, `[[rule.do]]` handler + args \
                — parsed by `boss_dispatcher::rules::registry::parse_raw_file`, validated by the \
                publish door's own `authoring::validate`, handler names checked against \
                `cascade::handler_emits` (this build's roster)",
        parse: parse_rules,
        scaffold: Some(scaffold_rules),
    },
    Entry {
        paths: &["seeds/subject_kinds.toml"],
        required: false,
        read_by: "NO READER (measured 2026-09-16). Check parses the rows conservatively",
        shape: "`[[subject_kind]]` rows: kind, label, description, owning_team, sort_order",
        parse: parse_subject_kinds,
        scaffold: None,
    },
    Entry {
        paths: &["seeds/accounts.toml"],
        required: false,
        read_by: "boss-brewery-engine, `include_str!` at compile time from examples/brewery/seeds \
                  — a copy in a tenant directory is never read",
        shape: "brewery engine data (`names`, `[[city]]`); check parses TOML only",
        parse: parse_toml_only,
        scaffold: None,
    },
    Entry {
        paths: &["seeds/vendors.toml"],
        required: false,
        read_by: "boss-brewery-engine, `include_str!` at compile time — never read from a tenant \
                  directory",
        shape: "brewery engine data (`[[vendor]]`); check parses TOML only",
        parse: parse_toml_only,
        scaffold: None,
    },
    Entry {
        paths: &["seeds/messages.toml"],
        required: false,
        read_by: "boss-brewery-engine, `include_str!` at compile time — never read from a tenant \
                  directory",
        shape: "brewery engine data (`[[thread]]`); check parses TOML only",
        parse: parse_toml_only,
        scaffold: None,
    },
    Entry {
        paths: &["seeds/bulletins.toml"],
        required: false,
        read_by: "boss-brewery-engine, `include_str!` at compile time — never read from a tenant \
                  directory",
        shape: "brewery engine data (`[[bulletin]]`); check parses TOML only",
        parse: parse_toml_only,
        scaffold: None,
    },
    Entry {
        paths: &["seeds/excise_rates.toml"],
        required: false,
        read_by: "boss-brewery-engine prepare (`seeds_dir/excise_rates.toml`)",
        shape: "brewery engine data (`effective_from`, `[[schedule]]`); check parses TOML only",
        parse: parse_toml_only,
        scaffold: None,
    },
    Entry {
        paths: &["seeds/parts.toml"],
        required: false,
        read_by: "boss-brewery-engine `load_parts` (raw-materials catalog + opening balances)",
        shape: "brewery engine data (`[[parts]]`); check parses TOML only",
        parse: parse_toml_only,
        scaffold: None,
    },
    Entry {
        paths: &["seeds/products.toml"],
        required: false,
        read_by: "NO READER (measured 2026-09-16): the brewery's finished-product catalog is \
                  hardcoded in its prepare, which says to keep it in sync with this file",
        shape: "brewery engine data (`[[products]]`); check parses TOML only",
        parse: parse_toml_only,
        scaffold: None,
    },
];

// ---------------------------------------------------------------------------
// Parsers — one per entry, each the product's own reader.
// ---------------------------------------------------------------------------

fn read(path: &Path) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))
}

fn parse_manifest(path: &Path, _: &Ctx) -> Result<String, String> {
    let t = TenantToml::parse(&read(path)?).map_err(|e| e.to_string())?;
    let id = t
        .meta
        .tenant_id
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| {
            "[meta] tenant_id is required: it is the owning_team every workflow is stamped with"
                .to_string()
        })?;
    Ok(format!(
        "tenant_id={id} display_name={}; {} modules, {} labels",
        t.meta.display_name.as_deref().unwrap_or("(unset)"),
        t.modules.len(),
        t.labels.len()
    ))
}

/// Top-level TOML keys other than `expected`, for the "parsed to
/// nothing" refusal: a file whose rows sit under a table name the
/// loader does not read would otherwise be a silent no-op.
fn stray_tables(text: &str, expected: &str) -> Result<Vec<String>, String> {
    let doc: toml::Table = toml::from_str(text).map_err(|e| e.to_string())?;
    Ok(doc
        .keys()
        .filter(|k| k.as_str() != expected)
        .cloned()
        .collect())
}

fn refuse_if_stray(text: &str, expected: &str, rows: usize) -> Result<(), String> {
    if rows > 0 {
        return Ok(());
    }
    let stray = stray_tables(text, expected)?;
    if stray.is_empty() {
        return Ok(());
    }
    let found: Vec<String> = stray.iter().map(|k| format!("[[{k}]]")).collect();
    Err(format!(
        "0 rows: the loader reads [[{expected}]] tables only; found {}",
        found.join(", ")
    ))
}

fn parse_workflows(path: &Path, ctx: &Ctx) -> Result<String, String> {
    let specs = boss_jobs::seed_loader::load_workflows_with_owning_team(path, &ctx.tenant_id)
        .map_err(|e| e.to_string())?;
    refuse_if_stray(&read(path)?, "workflow", specs.len())?;
    let kinds: Vec<&str> = specs.iter().map(|s| s.kind.as_str()).collect();
    Ok(match kinds.len() {
        0 => "0 workflows".to_string(),
        n => format!("{n} workflows: {}", kinds.join(", ")),
    })
}

fn parse_policy_rules(path: &Path, _: &Ctx) -> Result<String, String> {
    let rules =
        boss_policy_client::seed_loader::load_policy_rules(path).map_err(|e| format!("{e:#}"))?;
    refuse_if_stray(&read(path)?, "grants", rules.len())?;
    let roles: BTreeSet<&str> = rules.iter().map(|r| r.role.as_str()).collect();
    Ok(format!(
        "{} rules across {} roles",
        rules.len(),
        roles.len()
    ))
}

fn parse_classes(path: &Path, _: &Ctx) -> Result<String, String> {
    use boss_classes::http::ClassInput;
    let text = read(path)?;
    let rows: Vec<ClassInput> = if path.extension().and_then(|e| e.to_str()) == Some("toml") {
        #[derive(serde::Deserialize)]
        struct Bundle {
            #[serde(default)]
            class: Vec<ClassInput>,
        }
        let b: Bundle = toml::from_str(&text).map_err(|e| e.to_string())?;
        refuse_if_stray(&text, "class", b.class.len())?;
        b.class
    } else {
        serde_json::from_str(&text).map_err(|e| e.to_string())?
    };
    let kinds: BTreeSet<&str> = rows.iter().map(|r| r.subject_kind.as_str()).collect();
    Ok(format!(
        "{} classes over {} subject kinds",
        rows.len(),
        kinds.len()
    ))
}

/// The ledger door's own row type and validation (backlog 41af5195):
/// what publish sends is what check judged, so a row the door would
/// refuse — a kind outside the table's enum, a code declared twice, a
/// parent not declared before its child — is INVALID here, by the
/// door's own words.
fn parse_chart_of_accounts(path: &Path, _: &Ctx) -> Result<String, String> {
    let rows = boss_ledger::chart::load_chart_toml(path)?;
    refuse_if_stray(&read(path)?, "account", rows.len())?;
    Ok(match rows.len() {
        0 => "0 accounts".to_string(),
        n => format!(
            "{n} accounts: {}",
            rows.iter()
                .map(|a| match &a.parent {
                    Some(p) => format!(
                        "{} {} ({}/{}, under {p})",
                        a.code, a.name, a.kind, a.normal_balance
                    ),
                    None => format!("{} {} ({}/{})", a.code, a.name, a.kind, a.normal_balance),
                })
                .collect::<Vec<_>>()
                .join(", ")
        ),
    })
}

fn parse_employees(path: &Path, _: &Ctx) -> Result<String, String> {
    let rows: Vec<boss_people::Employee> =
        serde_json::from_str(&read(path)?).map_err(|e| e.to_string())?;
    Ok(format!("{} employees", rows.len()))
}

fn parse_operator_hires(path: &Path, _: &Ctx) -> Result<String, String> {
    #[derive(serde::Deserialize)]
    struct Bundle {
        #[serde(default)]
        hire: Vec<boss_people::Employee>,
    }
    let text = read(path)?;
    let b: Bundle = toml::from_str(&text).map_err(|e| e.to_string())?;
    refuse_if_stray(&text, "hire", b.hire.len())?;
    Ok(format!("{} hires", b.hire.len()))
}

fn parse_business_calendars(path: &Path, _: &Ctx) -> Result<String, String> {
    let rows: Vec<boss_core::calendar::BusinessCalendar> =
        serde_json::from_str(&read(path)?).map_err(|e| e.to_string())?;
    let codes: Vec<&str> = rows.iter().map(|c| c.code.as_str()).collect();
    Ok(format!("{} calendars: {}", rows.len(), codes.join(", ")))
}

fn parse_sensors(path: &Path, _: &Ctx) -> Result<String, String> {
    let rows = boss_jobs::sensors::load_sensors_toml(path)?;
    refuse_if_stray(&read(path)?, "sensor", rows.len())?;
    Ok(match rows.len() {
        0 => "0 sensors".to_string(),
        n => format!(
            "{n} sensors: {}",
            rows.iter()
                .map(|r| format!(
                    "{} ({} every {}m -> {})",
                    r.id, r.source, r.every_minutes, r.opens
                ))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    })
}

fn parse_posting_rules(path: &Path, _: &Ctx) -> Result<String, String> {
    let rows = boss_ledger::posting_rules::load_posting_rules_toml(path)?;
    refuse_if_stray(&read(path)?, "posting_rule", rows.len())?;
    Ok(match rows.len() {
        0 => "0 posting rules".to_string(),
        n => format!(
            "{n} posting rules: {}",
            rows.iter()
                .map(|r| format!(
                    "{} v{} ({}, {} lines)",
                    r.fact_kind,
                    r.version,
                    r.basis,
                    r.lines.len()
                ))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    })
}

fn parse_projection_rules(path: &Path, _: &Ctx) -> Result<String, String> {
    let rows = boss_ledger::posting_rules::load_projection_rules_toml(path)?;
    refuse_if_stray(&read(path)?, "projection", rows.len())?;
    Ok(match rows.len() {
        0 => "0 projections".to_string(),
        n => format!(
            "{n} projections: {}",
            rows.iter()
                .map(|r| format!(
                    "{}{} -> {}",
                    r.event_kind,
                    r.when
                        .as_ref()
                        .map(|w| format!(" when {w}"))
                        .unwrap_or_default(),
                    r.fact_kind
                ))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    })
}

fn parse_agents(path: &Path, _: &Ctx) -> Result<String, String> {
    let rows = boss_jobs::agents::load_agents_toml(path)?;
    refuse_if_stray(&read(path)?, "agent", rows.len())?;
    Ok(match rows.len() {
        0 => "0 agents".to_string(),
        n => format!(
            "{n} agents: {}",
            rows.iter()
                .map(|a| format!(
                    "{} ({}, {}; {} alias(es))",
                    a.id,
                    a.display_name,
                    a.default_model,
                    a.aliases.len()
                ))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    })
}

/// The batch door's own row type (backlog 1ec8312a): what publish
/// sends is what check judged, so a row the door would refuse is
/// INVALID here, by the door's own field name.
fn parse_locations(path: &Path, _: &Ctx) -> Result<String, String> {
    use boss_locations::http::LocationInput;
    #[derive(serde::Deserialize)]
    struct Bundle {
        #[serde(default)]
        location: Vec<LocationInput>,
    }
    let text = read(path)?;
    let b: Bundle = toml::from_str(&text).map_err(|e| e.to_string())?;
    refuse_if_stray(&text, "location", b.location.len())?;
    let rows: Vec<String> = b
        .location
        .iter()
        .map(|l| format!("{} ({}, {}, {})", l.id, l.name, l.kind, l.timezone))
        .collect();
    Ok(format!("{} locations: {}", rows.len(), rows.join("; ")))
}

/// The dispatcher's own reader (backlog 458971ef): the product
/// `[[rule]]` parser with its per-rule `why` guard, the publish door's
/// `validate` (topic, predicate, arg expressions, the trigger XOR), and
/// the handler roster the product's rules are pinned to. Handler names
/// are otherwise opaque to the dispatcher until dispatch, where an
/// unknown one is a loud `UnknownHandler` — so this is the one place a
/// tenant learns BEFORE publishing that its rule names a handler this
/// build does not carry (the real tenant's first rule names one a
/// sibling car is building, and reads INVALID here until it lands).
fn parse_rules(path: &Path, _: &Ctx) -> Result<String, String> {
    use boss_dispatcher::rules::{authoring, registry};
    let raw = registry::parse_raw_file(path).map_err(|e| e.to_string())?;
    refuse_if_stray(&read(path)?, "rule", raw.rules.len())?;
    let known = boss_dispatcher::cascade::handler_emits();
    let mut lines = Vec::new();
    for rule in &raw.rules {
        authoring::validate(rule).map_err(|e| e.to_string())?;
        for step in &rule.do_steps {
            if !known.contains_key(step.handler.as_str()) {
                return Err(format!(
                    "rule `{}` names handler `{}`, which this build of BOSS does not have \
                     (boss_dispatcher::cascade::handler_emits lists {} handlers); the publish \
                     door would accept it and the dispatcher would refuse it at dispatch as \
                     UnknownHandler",
                    rule.name,
                    step.handler,
                    known.len()
                ));
            }
        }
        let trigger = match (&rule.on_event, &rule.schedule) {
            (Some(topic), _) => topic.clone(),
            (None, Some(s)) => format!("schedule {}", s.cadence.token()),
            (None, None) => "no trigger".to_string(),
        };
        lines.push(format!(
            "{} v{} ({trigger} -> {})",
            rule.name,
            rule.version,
            rule.do_steps
                .iter()
                .map(|d| d.handler.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    Ok(match lines.len() {
        0 => "0 rules".to_string(),
        n => format!("{n} rules: {}", lines.join("; ")),
    })
}

fn parse_subject_kinds(path: &Path, _: &Ctx) -> Result<String, String> {
    #[derive(serde::Deserialize)]
    struct Row {
        kind: String,
        label: String,
    }
    #[derive(serde::Deserialize)]
    struct Bundle {
        #[serde(default)]
        subject_kind: Vec<Row>,
    }
    let text = read(path)?;
    let b: Bundle = toml::from_str(&text).map_err(|e| e.to_string())?;
    refuse_if_stray(&text, "subject_kind", b.subject_kind.len())?;
    let kinds: Vec<String> = b
        .subject_kind
        .iter()
        .map(|r| format!("{} ({})", r.kind, r.label))
        .collect();
    Ok(format!(
        "{} subject kinds: {} (no reader consumes this file today)",
        kinds.len(),
        kinds.join(", ")
    ))
}

fn parse_toml_only(path: &Path, _: &Ctx) -> Result<String, String> {
    let doc: toml::Table = toml::from_str(&read(path)?).map_err(|e| e.to_string())?;
    let keys: Vec<&str> = doc.keys().map(|k| k.as_str()).collect();
    Ok(format!(
        "parses as TOML ({}); tenant-engine data, not read by the platform",
        keys.join(", ")
    ))
}

// ---------------------------------------------------------------------------
// check
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Ok,
    Missing,
    Invalid,
    Unknown,
}

impl Status {
    fn label(self) -> &'static str {
        match self {
            Status::Ok => "OK",
            Status::Missing => "MISSING",
            Status::Invalid => "INVALID",
            Status::Unknown => "UNKNOWN",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// Relative to the tenant directory, as found (the alternate
    /// spelling when that is the one present).
    pub path: String,
    pub status: Status,
    pub detail: String,
}

#[derive(Debug, Default)]
pub struct Report {
    pub rows: Vec<Row>,
}

impl Report {
    /// Exit 0: nothing MISSING or INVALID. UNKNOWN files do not fail a
    /// check — they are reported so an extension is visible, not
    /// refused before the contract has a row for it.
    pub fn passed(&self) -> bool {
        !self
            .rows
            .iter()
            .any(|r| matches!(r.status, Status::Missing | Status::Invalid))
    }

    fn count(&self, s: Status) -> usize {
        self.rows.iter().filter(|r| r.status == s).count()
    }

    pub fn render(&self, dir: &Path) -> String {
        let width = self.rows.iter().map(|r| r.path.len()).max().unwrap_or(0);
        let mut out = format!("boss tenant check {}\n", dir.display());
        for r in &self.rows {
            // A loader's error can run to several lines (the seed lint
            // names each failing step); keep them under the detail
            // column rather than flattening the loader's words.
            let indent = format!("\n{:width$}", "", width = width + 13);
            out.push_str(&format!(
                "  {:<8} {:<width$}  {}\n",
                r.status.label(),
                r.path,
                r.detail.trim_end().replace('\n', &indent),
                width = width
            ));
        }
        out.push_str(&format!(
            "{} ok, {} missing, {} invalid, {} unknown — {}\n",
            self.count(Status::Ok),
            self.count(Status::Missing),
            self.count(Status::Invalid),
            self.count(Status::Unknown),
            if self.passed() { "PASS" } else { "FAIL" }
        ));
        out
    }
}

/// The `[meta] tenant_id` the directory declares, at either spelling;
/// a placeholder when it cannot be read so the workflow loader still
/// runs (the manifest row carries the real refusal).
fn tenant_id_of(dir: &Path) -> String {
    ["tenant.toml", "seeds/tenant.toml"]
        .iter()
        .map(|p| dir.join(p))
        .find(|p| p.is_file())
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|t| TenantToml::parse(&t).ok())
        .and_then(|t| t.meta.tenant_id)
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "tenant".to_string())
}

/// Validate `dir` against [`CONTRACT`]. Pure over the filesystem: it
/// reads, never writes, and never touches the network.
pub fn check(dir: &Path) -> Report {
    let ctx = Ctx {
        tenant_id: tenant_id_of(dir),
    };
    let mut rows = Vec::new();
    let mut named: BTreeSet<String> = BTreeSet::new();
    for e in CONTRACT {
        for p in e.paths {
            named.insert((*p).to_string());
        }
        let found = e.paths.iter().find(|p| dir.join(p).is_file());
        match found {
            Some(rel) => {
                let (status, detail) = match (e.parse)(&dir.join(rel), &ctx) {
                    Ok(d) => (Status::Ok, d),
                    Err(why) => (Status::Invalid, why),
                };
                rows.push(Row {
                    path: (*rel).to_string(),
                    status,
                    detail,
                });
            }
            None if e.required => rows.push(Row {
                path: e.paths[0].to_string(),
                status: Status::Missing,
                detail: "required".to_string(),
            }),
            None => {}
        }
    }
    // UNKNOWN: every regular file under seeds/, and every *.toml /
    // *.json at the root, that no entry names. Prose (README, DOMAIN)
    // and other directories (a tenant engine's data/) are not judged.
    let mut candidates: Vec<String> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir.join("seeds")) {
        candidates.extend(rd.filter_map(|e| e.ok()).filter_map(|e| {
            let p = e.path();
            p.is_file()
                .then(|| format!("seeds/{}", e.file_name().to_string_lossy()))
        }));
    }
    if let Ok(rd) = std::fs::read_dir(dir) {
        candidates.extend(rd.filter_map(|e| e.ok()).filter_map(|e| {
            let p = e.path();
            let ext = p.extension().and_then(|x| x.to_str());
            (p.is_file() && matches!(ext, Some("toml") | Some("json")))
                .then(|| e.file_name().to_string_lossy().to_string())
        }));
    }
    candidates.sort();
    rows.extend(
        candidates
            .into_iter()
            .filter(|c| !named.contains(c))
            .map(|c| Row {
                path: c,
                status: Status::Unknown,
                detail: "not named by the contract (docs/tenant-contract.md)".to_string(),
            }),
    );
    Report { rows }
}

// ---------------------------------------------------------------------------
// init
// ---------------------------------------------------------------------------

fn is_slug(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !name.starts_with('-')
        && !name.ends_with('-')
}

fn display_name_of(slug: &str) -> String {
    slug.split('-')
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut c = w.chars();
            match c.next() {
                Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Where `init` writes: `--into` when given, else `./<name>`.
fn target_dir(name: &str, into: Option<&Path>) -> PathBuf {
    into.map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from(name))
}

/// Write a tenant directory in the contract's shape. Refuses a
/// non-empty target so it can never overwrite a tenant. Returns every
/// path written, README first.
pub fn init(name: &str, into: Option<&Path>) -> Result<Vec<PathBuf>> {
    if !is_slug(name) {
        bail!(
            "`{name}` is not a slug: a tenant_id is lowercase letters, digits and hyphens \
             (it becomes every workflow's owning_team)"
        );
    }
    let dir = target_dir(name, into);
    if dir.exists() {
        if !dir.is_dir() {
            bail!("{} exists and is not a directory", dir.display());
        }
        let occupied = std::fs::read_dir(&dir)
            .with_context(|| format!("read {}", dir.display()))?
            .next()
            .is_some();
        if occupied {
            bail!(
                "{} is not empty: init writes a NEW tenant and never over an existing one",
                dir.display()
            );
        }
    }
    let s = Scaffold {
        name: name.to_string(),
        display_name: display_name_of(name),
    };
    let mut written = Vec::new();
    let mut put = |rel: &str, body: String| -> Result<()> {
        let path = dir.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create {}", parent.display()))?;
        }
        std::fs::write(&path, body).with_context(|| format!("write {}", path.display()))?;
        written.push(path);
        Ok(())
    };
    put("README.md", scaffold_readme(&s))?;
    for e in CONTRACT {
        if let Some(f) = e.scaffold {
            put(e.paths[0], f(&s))?;
        }
    }
    Ok(written)
}

// ---------------------------------------------------------------------------
// The contract as a table — docs/tenant-contract.md carries this
// between TABLE_BEGIN / TABLE_END, pinned equal by a test.
// ---------------------------------------------------------------------------

pub fn contract_table() -> String {
    let mut out = String::from(
        "| file | required | read by | shape | `init` writes it |\n|---|---|---|---|---|\n",
    );
    for e in CONTRACT {
        let files = e
            .paths
            .iter()
            .map(|p| format!("`{p}`"))
            .collect::<Vec<_>>()
            .join(" or ");
        out.push_str(&format!(
            "| {files} | {} | {} | {} | {} |\n",
            if e.required { "yes" } else { "no" },
            e.read_by,
            e.shape,
            if e.scaffold.is_some() { "yes" } else { "no" },
        ));
    }
    out
}

// ---------------------------------------------------------------------------
// Scaffold templates — each valid under its entry's parser, commented
// with what the file is and who reads it.
// ---------------------------------------------------------------------------

fn scaffold_readme(s: &Scaffold) -> String {
    let mut files = String::new();
    for e in CONTRACT.iter().filter(|e| e.scaffold.is_some()) {
        files.push_str(&format!(
            "- `{}` — {}. Read by: {}\n",
            e.paths[0],
            e.shape.split(';').next().unwrap_or(e.shape).trim(),
            e.read_by.split('(').next().unwrap_or(e.read_by).trim()
        ));
    }
    // A raw literal: a `\`-continued string strips the next line's
    // leading spaces, which is exactly the markdown indent a wrapped
    // bullet needs.
    format!(
        r#"# {display} — a BOSS tenant

This directory is **{display}** described as a tenant of BOSS: what the
company is (its taxonomy, people, calendars, access rules) and how it
operates (its protocols). It lives OUTSIDE the product tree, in exactly
the shape the product reads: `tenant.toml` (the manifest) + `seeds/`.
The product's `examples/brewery` and `examples/used-device-shop`
exercise the same contract publicly.

Scaffolded by `boss tenant init {name}`. Validate it any time with
`boss tenant check .` — it judges every file with the product's own
loader and reports OK / MISSING / INVALID / UNKNOWN per file.

## The files

{files}
The full contract — every file the product reads from a tenant
directory, who reads it and its shape — is `docs/tenant-contract.md`
in the product repo.

## Rules

- **No secrets, ever.** Credentials live in the deployment's secret
  store and are referenced by name.
- Business data that is *work* (an order, a request, an invoice) is
  packets in the system of record, not files here. This directory holds
  *declarations* — what the company is and how it operates — so a change
  to it is a pull request with a diff to approve.
- A protocol grows by publishing a new `[[workflow]]` version, never by
  editing the product.
"#,
        display = s.display_name,
        name = s.name,
    )
}

fn scaffold_manifest(s: &Scaffold) -> String {
    format!(
        "# {display} — the tenant manifest.\n\
#\n\
# Read by the gateway (/api/tenant/manifest, inlined into index.html so\n\
# the first paint knows the tenant). The deployment points\n\
# BOSS_TENANT_MANIFEST_TOML at this file (BOSS_TENANT_DIR at the\n\
# directory once fcc1d57b car 2 lands). Unknown sections are ignored,\n\
# which is where a simulated tenant's [job_rates] etc. ride.\n\
\n\
[meta]\n\
# The slug every workflow is stamped with as owning_team. Lowercase,\n\
# digits and hyphens.\n\
tenant_id = \"{name}\"\n\
# What the SPA calls this deployment.\n\
display_name = \"{display}\"\n\
\n\
# Which SPA modules to show. Absent = all shown; set one to false to hide it.\n\
[modules]\n\
\n\
# Display strings keyed by dotted path, e.g.\n\
#   \"finance.revenue_category.sponsorship\" = \"Sponsorship\"\n\
# becomes a row in /api/finance/revenue-categories.\n\
[labels]\n",
        display = s.display_name,
        name = s.name,
    )
}

fn scaffold_workflows(s: &Scaffold) -> String {
    format!(
        "# {display} — the company's own protocols.\n\
#\n\
# Read by boss-jobs' seed loader (with its viability lint: every\n\
# workflow needs >= 1 trigger step and >= 1 terminal, every predicate\n\
# must resolve, the graph must be acyclic and reachable), published by\n\
# the tenant prepare, and counted by the live-protocols lint. Platform\n\
# protocols (ship-a-change, backlog-item, ...) ship with the product;\n\
# this file carries only what is {display}'s. Grow a protocol by\n\
# publishing a new version — never by a deploy.\n\
#\n\
# The DAG is implicit: a step's `ready_when` names the steps before it.\n\
\n\
[[workflow]]\n\
kind = \"handle-a-request\"\n\
label = \"Handle a request\"\n\
category = \"operations\"\n\
subject_kinds = [\"custom\"]\n\
description = \"A request arrives, the owner handles it, and the outcome is recorded. Replace this with the first thing {display} actually does.\"\n\
\n\
[[workflow.step]]\n\
title = \"received\"\n\
kind = \"trigger\"\n\
ready_when = \"true\"\n\
title_template = \"Request received\"\n\
metadata_defaults = {{ trigger_kind = \"operator\", trigger_name = \"request\" }}\n\
\n\
[[workflow.step]]\n\
title = \"handle\"\n\
kind = \"task\"\n\
ready_when = \"steps.received.done\"\n\
audience = {{ role = \"owner\" }}\n\
title_template = \"Handle the request\"\n\
fields = [{{ name = \"result\", field_type = \"string\", required = true }}]\n\
\n\
[[workflow.step]]\n\
title = \"handled\"\n\
kind = \"outcome\"\n\
ready_when = \"steps.handle.done\"\n\
title_template = \"Request handled\"\n\
metadata_defaults = {{ outcome_kind = \"completed\" }}\n\
terminal = {{ outcome = \"handled\" }}\n",
        display = s.display_name,
    )
}

fn scaffold_policy_rules(s: &Scaffold) -> String {
    format!(
        "# {display} — tenant policy seed.\n\
#\n\
# Read by boss-policy-bootstrap at first boot (via\n\
# boss_policy_client::seed_loader): each [[grants]] row expands to one\n\
# rule per role x resource x action. Core ships the platform rules\n\
# (platform-admin / audit-readonly / smoke-tester / guest); every\n\
# business role is granted here. Scope: all / self / team / territory /\n\
# none / department:<name>. Actions: read, create, update, close,\n\
# sign-off, delete, publish, retire.\n\
\n\
[[grants]]\n\
role = \"owner\"\n\
resources = [\"job\", \"step\", \"employee\", \"account\", \"workflow\", \"policy-rule\"]\n\
actions = [\"read\", \"create\", \"update\", \"close\", \"sign-off\", \"publish\"]\n\
scope = \"all\"\n",
        display = s.display_name,
    )
}

/// JSON has no comments; the README and the contract doc say what this
/// file is — the tenant's Class registry rows (departments, roles,
/// account types), POSTed to /api/classes/batch by the tenant prepare.
fn scaffold_classes(_: &Scaffold) -> String {
    concat!(
        "[\n",
        "  {\"subject_kind\": \"employee\", \"code\": \"operations\", \"display_name\": \"Operations\", \"member_attribute\": \"department\", \"sort_order\": 10},\n",
        "  {\"subject_kind\": \"employee\", \"code\": \"owner\", \"display_name\": \"Owner\", \"member_attribute\": \"role\", \"sort_order\": 1},\n",
        "  {\"subject_kind\": \"account\", \"code\": \"customer\", \"display_name\": \"Customer\", \"member_attribute\": \"account_type\", \"sort_order\": 10}\n",
        "]\n"
    )
    .to_string()
}

fn scaffold_chart_of_accounts(s: &Scaffold) -> String {
    format!(
        "# {display} — chart of accounts (backlog 41af5195; design 18cf4272).\n\
#\n\
# The chart is yours: one [[account]] per row of the ledger's\n\
# `gl_accounts` table — code, name, kind (asset | liability | equity |\n\
# revenue | expense), normal_balance (debit | credit), and an optional\n\
# parent naming a code declared EARLIER in this file. Published by\n\
# `boss tenant publish` (POST /api/ledger/accounts/batch) after the\n\
# classes, insert-if-absent by code. The product ships a starter chart\n\
# (the OSS default the demo tenant runs on); a code it already holds is\n\
# the SAME account, kept under the starter's name, and the publish line\n\
# names the field this file differs on — adopt the code as it stands or\n\
# choose another. Nothing renames an account in place: the code is what\n\
# every posting rule and journal line points at.\n\
#\n\
# [[account]]\n\
# code = \"1000\"\n\
# name = \"Bank\"\n\
# kind = \"asset\"\n\
# normal_balance = \"debit\"\n\
#\n\
# [[account]]\n\
# code = \"1010\"\n\
# name = \"Stripe balance\"\n\
# kind = \"asset\"\n\
# normal_balance = \"debit\"\n\
# parent = \"1000\"\n",
        display = s.display_name
    )
}

fn scaffold_employees(s: &Scaffold) -> String {
    format!(
        r#"[
  {{
    "id": "emp-owner",
    "name": "{display} owner",
    "email": "owner@{name}.example",
    "role": "owner",
    "department": "operations",
    "skill_level": null,
    "hire_date": "2026-01-01",
    "location": null,
    "manager_id": null,
    "employment_type": "full-time",
    "status": "active",
    "skills": [],
    "certifications": [],
    "annual_salary_cents": null
  }}
]
"#,
        display = s.display_name,
        name = s.name,
    )
}

fn scaffold_business_calendars(_: &Scaffold) -> String {
    "[\n  {\"code\": \"default\", \"name\": \"Default business calendar (Mon-Fri)\", \"weekend\": [5, 6], \"closed\": []}\n]\n"
        .to_string()
}

fn scaffold_sensors(s: &Scaffold) -> String {
    format!(
        "# {display} — sensors (design 14c9b2ad).\n\
#\n\
# A sensor is a reading of the world outside BOSS, polled on a cadence\n\
# by the platform and turned into a packet of the kind you declare.\n\
# The product knows how to read a `source`; WHAT to open for it is\n\
# yours. `credential` names a `credentials` registry row whose value\n\
# the deployment holds (never this file). Published to the `sensors`\n\
# registry by `boss tenant publish`, insert-if-absent by id.\n\
#\n\
# [[sensor]]\n\
# id = \"stripe-sponsorships\"\n\
# source = \"stripe\"\n\
# credential = \"stripe-restricted-read\"\n\
# every_minutes = 15\n\
# opens = \"receive-a-sponsorship\"\n\
# subject_kind = \"custom\"\n\
#\n\
# A push-only source is one nothing polls: the gateway's site surface\n\
# records one reading per page view on `www-visits` itself, so the\n\
# row names no credential and no period.\n\
#\n\
# [[sensor]]\n\
# id = \"www-visits\"\n\
# source = \"site\"\n\
# opens = \"marketing-weekly\"\n\
# subject_kind = \"custom\"\n",
        display = s.display_name
    )
}

fn scaffold_posting_rules(s: &Scaffold) -> String {
    format!(
        "# {display} — posting rules (backlog a40541cb).\n\
#\n\
# A posting rule turns one kind of financial fact into journal lines.\n\
# The product's code rules cover invoices, bills, payroll and the rest;\n\
# a fact kind that is YOURS is declared here and evaluated by the\n\
# newest version of its rule (a change is the next version, never an\n\
# edit). `amount_path` is a JSON pointer into the fact's payload,\n\
# integer cents; the debit pointers and the credit pointers must match\n\
# as a multiset, so the entry balances for every fact. `basis` is the\n\
# accounting basis you declare (cash | accrual), recorded, not a code\n\
# path. Published to the ledger by `boss tenant publish`\n\
# (POST /api/ledger/posting-rules/batch, insert-if-absent by\n\
# fact_kind + version, source = tenant:<id>).\n\
#\n\
# [[posting_rule]]\n\
# fact_kind = \"finance.sponsorship.received\"\n\
# basis = \"cash\"\n\
# lines = [\n\
#   {{ account_code = \"1010\", side = \"debit\",  amount_path = \"/metadata/amount_cents\", memo = \"Sponsorship {{/job_id}}\" }},\n\
#   {{ account_code = \"4100\", side = \"credit\", amount_path = \"/metadata/amount_cents\" }},\n\
#   {{ account_code = \"6100\", side = \"debit\",  amount_path = \"/metadata/fee_cents\" }},\n\
#   {{ account_code = \"1010\", side = \"credit\", amount_path = \"/metadata/fee_cents\" }},\n\
# ]\n",
        display = s.display_name
    )
}

fn scaffold_projection_rules(s: &Scaffold) -> String {
    format!(
        "# {display} — event -> fact projections (backlog a40541cb).\n\
#\n\
# A projection turns an audit_log event into a financial fact the\n\
# posting rules then post. `when` is a table of {{\"/pointer\" = value}}\n\
# and every pointer must equal its value for the rule to fire — how ONE\n\
# workflow's completed step is picked out of the `step.done.task` every\n\
# workflow emits (a step.done payload carries workflow_kind, spec_slug,\n\
# job_id, completed_on and the step's metadata). Published to the\n\
# ledger by `boss tenant publish` (POST\n\
# /api/ledger/fact-projection-rules/batch, insert-if-absent by\n\
# event_kind + when).\n\
#\n\
# [[projection]]\n\
# event_kind = \"step.done.task\"\n\
# when = {{ \"/workflow_kind\" = \"receive-a-sponsorship\", \"/spec_slug\" = \"recognize\" }}\n\
# fact_kind = \"finance.sponsorship.received\"\n\
# source_table = \"jobs\"\n\
# source_id_path = \"/job_id\"\n\
# happened_on_path = \"/completed_on\"\n",
        display = s.display_name
    )
}

fn scaffold_agents(s: &Scaffold) -> String {
    format!(
        "# {display} — registered agents (design 6fda05ae; backlog f56155f0).\n\
#\n\
# An agent is an actor, not a person: the machine half of the roster\n\
# the employees file is the human half of. Its canonical id is\n\
# agent-<slug>; the addresses it logs in with are `aliases` of that id\n\
# (the jobs API resolves each one at its door); `default_model` is\n\
# what a run uses when it does not say, spelled as the rate card\n\
# spells it (opus-5[1m], never claude-…); the caps are the budget the\n\
# jobs API admits each run against, and are unset until measured.\n\
# Published to the agents registry by `boss tenant publish`,\n\
# insert-if-absent by id: a row the platform already registered is\n\
# kept, and the publish names any field this file differs on.\n\
#\n\
# [[agent]]\n\
# id = \"agent-scout\"\n\
# display_name = \"Scout (research)\"\n\
# default_model = \"sonnet-5\"\n\
# aliases = [\"scout@{name}.example\"]\n",
        display = s.display_name,
        name = s.name,
    )
}

fn scaffold_rules(s: &Scaffold) -> String {
    format!(
        "# {display} — dispatcher rules (backlog 458971ef).\n\
#\n\
# A rule is a reaction the platform runs for you when an event lands\n\
# (or on a schedule): the same [[rule]] shape the product's own rules\n\
# take under infra/dispatcher/rules/, with a `why` naming which\n\
# standing exemption it claims — timer, threshold, external glue, or\n\
# cross-protocol reactor. If the reaction belongs to ONE protocol,\n\
# declare it in that workflow instead. Published by `boss tenant\n\
# publish` through the dispatcher's own door, append-only: bump\n\
# `version` to change a rule; an unchanged version is a no-op. Each\n\
# row lands as source = tenant:{name}, which the product's boot seed\n\
# leaves alone — a rule declared here survives every converge.\n\
#\n\
# [[rule]]\n\
# name = \"complete-site-live-on-converge-closed\"\n\
# why = \"\"\"\n\
# A cross-protocol reactor: the site's `live` step completes from the\n\
# converge packet's record, which no single workflow can express.\n\
# \"\"\"\n\
# version = 1\n\
# on_event = \"jobs.job.closed\"\n\
# when = 'kind = \"maintenance-cluster-converge\"'\n\
# [[rule.do]]\n\
# handler = \"jobs.complete_step_matching\"\n\
# args = {{ kind = '\"publish-the-landing-page\"', step = '\"live\"' }}\n",
        display = s.display_name,
        name = s.name,
    )
}

fn scaffold_locations(s: &Scaffold) -> String {
    format!(
        "# {display} — locations.\n\
#\n\
# One [[location]] per row of the `locations` table (id, name, kind,\n\
# timezone; parent_id / latitude / longitude / address / metadata are\n\
# optional). Published to the registry by `boss tenant publish`\n\
# (POST /api/locations/batch, insert-if-absent by id) BEFORE the\n\
# roster, because an employee's `location` must name a row here.\n\
\n\
[[location]]\n\
id = \"loc-{name}-hq\"\n\
name = \"{display} HQ\"\n\
kind = \"remote\"\n\
timezone = \"UTC\"\n",
        display = s.display_name,
        name = s.name,
    )
}

// ---------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------

#[derive(clap::Subcommand)]
pub enum Cmd {
    /// A tenant is a directory of its own: scaffold one, check one, or
    /// print the contract they are judged against.
    #[command(subcommand)]
    Tenant(TenantAction),
}

#[derive(clap::Subcommand)]
pub enum TenantAction {
    /// Write a new tenant directory in the shape the product reads
    /// (tenant.toml + seeds/), refusing a non-empty target.
    Init {
        /// The tenant_id: lowercase letters, digits and hyphens.
        name: String,
        /// Directory to write into (default: ./<name>).
        #[arg(long)]
        into: Option<PathBuf>,
    },
    /// Validate a tenant directory with the product's own loaders.
    /// Exit 0 when nothing is MISSING or INVALID.
    Check { dir: PathBuf },
    /// Print the contract table (what docs/tenant-contract.md carries).
    Contract,
    /// Publish a tenant directory into a running deployment through
    /// the shared doors, in the engines' dependency order, idempotently
    /// (ee7b62bb). Refuses a directory that fails `check`.
    Publish {
        dir: PathBuf,
        /// Route every /api prefix through one gateway URL (default:
        /// each service's own localhost port, the in-pod launcher path).
        #[arg(long)]
        gateway: Option<String>,
        /// Print every write the publish WOULD make; no HTTP.
        #[arg(long)]
        dry_run: bool,
    },
}

pub async fn dispatch(cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::Tenant(TenantAction::Init { name, into }) => {
            let written = init(&name, into.as_deref())?;
            for p in &written {
                println!("wrote {}", p.display());
            }
            println!(
                "{} files; validate with: boss tenant check {}",
                written.len(),
                written
                    .first()
                    .and_then(|p| p.parent())
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| name.clone())
            );
            Ok(())
        }
        Cmd::Tenant(TenantAction::Check { dir }) => {
            let r = check(&dir);
            print!("{}", r.render(&dir));
            if r.passed() {
                Ok(())
            } else {
                std::process::exit(1)
            }
        }
        Cmd::Tenant(TenantAction::Contract) => {
            // With the markers, so the output pastes into
            // docs/tenant-contract.md verbatim.
            print!("{TABLE_BEGIN}\n{}{TABLE_END}\n", contract_table());
            Ok(())
        }
        Cmd::Tenant(TenantAction::Publish {
            dir,
            gateway,
            dry_run,
        }) => {
            let plan = crate::tenant_publish::plan(&dir)?;
            let bases = crate::tenant_publish::Bases::resolve(gateway.as_deref());
            println!("{}", plan.render_header(dry_run));
            println!("{}", bases.describe(gateway.as_deref()));
            if dry_run {
                for s in &plan.steps {
                    println!("{}", plan.render_step(s, None));
                }
                println!("{}", plan.render_footer());
                if plan.publishable() {
                    return Ok(());
                }
                std::process::exit(1)
            }
            // The doors are blocking reqwest (the engines call them from
            // a plain main); under this async main they run on a
            // blocking thread, printing each line as it lands so a
            // launcher log shows where a cold stack is holding.
            tokio::task::spawn_blocking(move || {
                crate::tenant_publish::publish(&plan, &bases, &mut |l| println!("{l}"))?;
                println!("{}", plan.render_footer());
                Ok(())
            })
            .await?
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use boss_testing::scratch::{scratch_dir, write_file};

    fn status_of<'a>(r: &'a Report, path: &str) -> Option<&'a Row> {
        r.rows.iter().find(|row| row.path == path)
    }

    /// Write `rel` under `dir`, creating `seeds/` as needed.
    fn put(dir: &Path, rel: &str, body: &str) {
        let path = dir.join(rel);
        if let Some(parent) = path.parent() {
            boss_testing::scratch::create_dir(parent);
        }
        write_file(&path, body);
    }

    fn assert_all_ok(dir: &Path) {
        let r = check(dir);
        let bad: Vec<&Row> = r
            .rows
            .iter()
            .filter(|row| matches!(row.status, Status::Missing | Status::Invalid))
            .collect();
        assert!(
            bad.is_empty() && r.passed(),
            "{} does not pass the contract:\n{:#?}",
            dir.display(),
            bad
        );
    }

    /// Every example ships a valid tenant — the product's public
    /// exercise of the contract. A new example must pass too.
    #[test]
    fn every_example_tenant_passes_check() {
        let root = boss_testing::repo_root();
        let examples: Vec<PathBuf> = std::fs::read_dir(root.join("examples"))
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.join("seeds").is_dir())
            .collect();
        assert!(examples.len() >= 2, "expected brewery + used-device-shop");
        for ex in examples {
            assert_all_ok(&ex);
        }
    }

    #[test]
    fn the_brewery_has_no_unknown_files() {
        // The brewery bundle is the reference shape: every file in it
        // is named by the contract (its engine-only TOMLs included).
        let r = check(&boss_testing::repo_root().join("examples/brewery"));
        let unknown: Vec<&Row> = r
            .rows
            .iter()
            .filter(|row| row.status == Status::Unknown)
            .collect();
        assert!(unknown.is_empty(), "{unknown:#?}");
    }

    #[test]
    fn a_fresh_init_passes_check_and_names_every_scaffolded_file() {
        let dir = scratch_dir("boss-cli-tenant-init-passes").join("acme");
        let written = init("acme", Some(&dir)).unwrap();
        assert!(dir.join("tenant.toml").is_file());
        assert!(dir.join("seeds/workflows.toml").is_file());
        assert!(dir.join("README.md").is_file());
        assert!(written.len() >= 4, "{written:?}");
        assert_all_ok(&dir);
        let r = check(&dir);
        assert!(
            r.rows.iter().all(|row| row.status == Status::Ok),
            "a scaffold must be entirely OK, not merely passing:\n{:#?}",
            r.rows
        );
        let readme = std::fs::read_to_string(dir.join("README.md")).unwrap();
        for p in &written {
            let rel = p.strip_prefix(&dir).unwrap().display().to_string();
            if rel != "README.md" {
                assert!(readme.contains(&rel), "README does not name {rel}");
            }
        }
    }

    #[test]
    fn init_defaults_the_directory_to_the_name() {
        // Pure: cwd is process-wide and the other tests read it
        // (`repo_root` refuses a cwd outside the compiled tree), so the
        // rule is tested as a function, not by chdir.
        assert_eq!(target_dir("north-star", None), PathBuf::from("north-star"));
        let into = PathBuf::from("/elsewhere/x");
        assert_eq!(target_dir("north-star", Some(&into)), into);
    }

    #[test]
    fn init_refuses_a_non_empty_directory() {
        let dir = scratch_dir("boss-cli-tenant-init-refuses-nonempty");
        write_file(&dir.join("something.txt"), "already here\n");
        let err = init("acme", Some(&dir)).unwrap_err().to_string();
        assert!(err.contains("not empty"), "{err}");
        assert!(
            !dir.join("tenant.toml").exists(),
            "refusal must write nothing"
        );
    }

    #[test]
    fn init_refuses_a_name_that_is_not_a_slug() {
        let dir = scratch_dir("boss-cli-tenant-init-refuses-name").join("x");
        let err = init("Acme Inc", Some(&dir)).unwrap_err().to_string();
        assert!(err.contains("slug"), "{err}");
    }

    /// The real tenant's shape (david/algedonic-llc @ 7619cfc,
    /// 2026-09-16), copied as a fixture — a test never reads the
    /// private repo. Every file the contract names is judged by the
    /// product's own loader — including agents.toml, which was UNKNOWN
    /// (the first extension the real tenant asked for) until its seed
    /// landed (f56155f0, 2026-09-17): in the contract's shape it is OK;
    /// in the real tenant's first-draft shape, carrying `role` and
    /// `department` the registry cannot hold, it is INVALID naming the
    /// field rather than silently dropping it.
    #[test]
    fn the_real_tenants_shape_is_judged_file_by_file() {
        let dir = scratch_dir("boss-cli-tenant-check-algedonic-shape");
        write_file(
            &dir.join("tenant.toml"),
            "[meta]\ntenant_id = \"algedonic\"\ndisplay_name = \"Algedonic, LLC\"\n\
             operating_days = [\"mon\"]\ntimezone = \"America/Los_Angeles\"\n",
        );
        write_file(&dir.join("README.md"), "# prose is not checked\n");
        write_file(&dir.join("DOMAIN.md"), "# prose is not checked\n");
        let seeds = dir.join("seeds");
        boss_testing::scratch::create_dir(&seeds);
        write_file(
            &seeds.join("agents.toml"),
            "[[agent]]\nid = \"agent-claude\"\ndisplay_name = \"Claude (engineering)\"\n\
             default_model = \"opus-5[1m]\"\naliases = [\"claude@algedonic.dev\"]\n",
        );
        write_file(
            &seeds.join("classes.json"),
            r#"[
  {"subject_kind": "employee", "code": "engineering", "display_name": "Engineering", "member_attribute": "department", "sort_order": 10},
  {"subject_kind": "employee", "code": "founder", "display_name": "Founder", "member_attribute": "role", "sort_order": 1}
]"#,
        );
        write_file(
            &seeds.join("employees.json"),
            r#"[{"id": "emp-david", "name": "David Auld", "email": "david@algedonic.dev",
  "github_username": "dauld", "role": "founder", "department": "operations", "skill_level": null,
  "hire_date": "2026-09-16", "location": "loc-algedonic-hq", "manager_id": null,
  "employment_type": "full-time", "status": "active", "skills": [], "certifications": [],
  "annual_salary_cents": 0}]"#,
        );
        write_file(
            &seeds.join("locations.toml"),
            "[[location]]\nid = \"loc-algedonic-hq\"\nname = \"HQ\"\nkind = \"office\"\ntimezone = \"America/Los_Angeles\"\n",
        );
        // The real file's shape: `[[rule]]` with `why`, not the
        // loader's `[[grants]]`. The loader reads zero grants from it.
        write_file(
            &seeds.join("policy_rules.toml"),
            "[[rule]]\nrole = \"founder\"\naction = \"*\"\nresource = \"*\"\nscope = \"all\"\nwhy = \"one human\"\n",
        );
        // The real file's shape: `id`/`working_days`, not the batch
        // endpoint's `code`/`weekend`/`closed`.
        write_file(
            &seeds.join("business_calendars.json"),
            r#"[{"id": "cal-algedonic", "name": "founder hours", "timezone": "America/Los_Angeles",
  "working_days": ["mon"], "day_start": "09:00", "day_end": "18:00"}]"#,
        );
        write_file(
            &seeds.join("workflows.toml"),
            r#"[[workflow]]
kind = "receive-a-sponsorship"
label = "Receive a sponsorship"
category = "sales"
subject_kinds = ["account"]
metadata = { owner_role = "founder", department = "sales" }

[[workflow.step]]
title = "received"
kind = "trigger"
ready_when = "true"
title_template = "Stripe reported a payment"
metadata_defaults = { trigger_kind = "sensor", trigger_name = "stripe-checkout-completed" }

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
"#,
        );

        // The first sensor (design 14c9b2ad), in the real file's shape.
        write_file(
            &seeds.join("sensors.toml"),
            "[[sensor]]\nid = \"stripe-sponsorships\"\nsource = \"stripe\"\n\
             credential = \"stripe-restricted-read\"\nevery_minutes = 15\n\
             opens = \"receive-a-sponsorship\"\nsubject_kind = \"custom\"\n",
        );

        // The tenant's own reactor (backlog 458971ef), in the product
        // rule file's shape. The real tenant's rule names the sibling
        // car's `jobs.complete_step_matching`; this fixture proves the
        // mechanics with a handler every build has.
        write_file(
            &seeds.join("rules.toml"),
            "[[rule]]\nname = \"complete-site-live-on-converge-closed\"\n\
             why = \"\"\"\nA cross-protocol reactor: the converge packet's close is the site's evidence.\n\"\"\"\n\
             version = 2\non_event = \"jobs.job.closed\"\n\
             when = 'kind = \"maintenance-cluster-converge\"'\n\
             [[rule.do]]\nhandler = \"messages.notify\"\n",
        );

        let r = check(&dir);
        for ok in [
            "tenant.toml",
            "seeds/agents.toml",
            "seeds/classes.json",
            "seeds/employees.json",
            "seeds/locations.toml",
            "seeds/sensors.toml",
            "seeds/rules.toml",
        ] {
            let row = status_of(&r, ok).unwrap_or_else(|| panic!("{ok} is reported"));
            assert_eq!(row.status, Status::Ok, "{row:?}");
        }
        let rules = status_of(&r, "seeds/rules.toml").unwrap();
        assert!(
            rules
                .detail
                .contains("complete-site-live-on-converge-closed v2")
                && rules.detail.contains("jobs.job.closed")
                && rules.detail.contains("messages.notify"),
            "{rules:?}"
        );
        let agents = status_of(&r, "seeds/agents.toml").unwrap();
        assert!(
            agents.detail.contains("agent-claude") && agents.detail.contains("opus-5[1m]"),
            "{agents:?}"
        );
        // The real tenant's first draft (20f3a9e) carried role and
        // department: a field the agents registry cannot hold is
        // refused by name, never dropped.
        write_file(
            &seeds.join("agents.toml"),
            "[[agent]]\nid = \"agent-claude\"\ndisplay_name = \"Claude (engineering)\"\n\
             default_model = \"opus-5[1m]\"\naliases = [\"claude@algedonic.dev\"]\n\
             role = \"engineering-agent\"\ndepartment = \"engineering\"\n",
        );
        let drafted = check(&dir);
        let agents = status_of(&drafted, "seeds/agents.toml").unwrap();
        assert_eq!(agents.status, Status::Invalid, "{agents:?}");
        // toml names one unknown field per refusal (the last it read).
        assert!(
            agents.detail.contains("unknown field")
                && (agents.detail.contains("`role`") || agents.detail.contains("`department`")),
            "{agents:?}"
        );
        // Three files the product would refuse or silently mis-read
        // are named, with the loader's own words. The workflow's
        // trigger declares `trigger_kind = "sensor"`, a value outside
        // the StepType's `periodic|event|operator|counterparty` — the
        // viability lint refuses it, so the publish would too. This
        // is the first thing `check` found on the real tenant.
        let wf = status_of(&r, "seeds/workflows.toml").unwrap();
        assert_eq!(wf.status, Status::Invalid, "{wf:?}");
        assert!(wf.detail.contains("sensor"), "{wf:?}");
        // A policy file with no grants, and a calendar in a shape the
        // batch endpoint rejects.
        let policy = status_of(&r, "seeds/policy_rules.toml").unwrap();
        assert_eq!(policy.status, Status::Invalid, "{policy:?}");
        assert!(policy.detail.contains("[[rule]]"), "{policy:?}");
        let cal = status_of(&r, "seeds/business_calendars.json").unwrap();
        assert_eq!(cal.status, Status::Invalid, "{cal:?}");
        assert!(cal.detail.contains("code"), "{cal:?}");
        assert!(!r.passed());
        // Prose is not a file the contract judges.
        assert!(status_of(&r, "README.md").is_none());
    }

    /// A sensor declaration is judged by the product's own loader: a
    /// bad row is INVALID naming the row, and rows under the wrong
    /// table name parse to nothing and are refused rather than passed.
    #[test]
    fn a_bad_sensor_row_is_invalid_by_name() {
        let dir = scratch_dir("boss-cli-tenant-check-sensors");
        write_file(&dir.join("tenant.toml"), "[meta]\ntenant_id = \"t\"\n");
        let seeds = dir.join("seeds");
        boss_testing::scratch::create_dir(&seeds);
        write_file(&seeds.join("workflows.toml"), "");
        write_file(
            &seeds.join("sensors.toml"),
            "[[sensor]]\nid = \"stripe-sponsorships\"\nsource = \"stripe\"\n\
             credential = \"stripe-restricted-read\"\nevery_minutes = 0\n\
             opens = \"receive-a-sponsorship\"\nsubject_kind = \"custom\"\n",
        );
        let r = check(&dir);
        let row = status_of(&r, "seeds/sensors.toml").unwrap();
        assert_eq!(row.status, Status::Invalid, "{row:?}");
        assert!(row.detail.contains("stripe-sponsorships"), "{row:?}");
        assert!(row.detail.contains("every_minutes"), "{row:?}");

        write_file(&seeds.join("sensors.toml"), "[[sensors]]\nid = \"x\"\n");
        let r = check(&dir);
        let row = status_of(&r, "seeds/sensors.toml").unwrap();
        assert_eq!(row.status, Status::Invalid, "{row:?}");
        assert!(row.detail.contains("[[sensor]]"), "{row:?}");
    }
    /// The two ledger rule files (backlog a40541cb) are judged by the
    /// ledger's own loaders: an unbalanced posting rule is INVALID
    /// naming the rule, a projection whose `when` key is not a pointer
    /// is INVALID naming the key, and a stray table name is refused.
    #[test]
    fn a_bad_ledger_rule_row_is_invalid_by_name() {
        let dir = scratch_dir("boss-cli-tenant-check-ledger-rules");
        write_file(&dir.join("tenant.toml"), "[meta]\ntenant_id = \"t\"\n");
        let seeds = dir.join("seeds");
        boss_testing::scratch::create_dir(&seeds);
        write_file(&seeds.join("workflows.toml"), "");
        write_file(
            &seeds.join("posting_rules.toml"),
            "[[posting_rule]]\nfact_kind = \"finance.sponsorship.received\"\nbasis = \"cash\"\n\
             lines = [\n\
               { account_code = \"1010\", side = \"debit\", amount_path = \"/amount_cents\" },\n\
               { account_code = \"4100\", side = \"credit\", amount_path = \"/amount_cents\" },\n\
               { account_code = \"6100\", side = \"debit\", amount_path = \"/fee_cents\" },\n\
             ]\n",
        );
        write_file(
            &seeds.join("fact_projection_rules.toml"),
            "[[projection]]\nevent_kind = \"step.done.task\"\n\
             when = { \"spec_slug\" = \"recognize\" }\n\
             fact_kind = \"finance.sponsorship.received\"\nsource_table = \"jobs\"\n\
             source_id_path = \"/job_id\"\n",
        );
        let r = check(&dir);
        let row = status_of(&r, "seeds/posting_rules.toml").unwrap();
        assert_eq!(row.status, Status::Invalid, "{row:?}");
        assert!(
            row.detail.contains("finance.sponsorship.received v1"),
            "{row:?}"
        );
        assert!(row.detail.contains("not balanced"), "{row:?}");
        let row = status_of(&r, "seeds/fact_projection_rules.toml").unwrap();
        assert_eq!(row.status, Status::Invalid, "{row:?}");
        assert!(row.detail.contains("spec_slug"), "{row:?}");

        write_file(
            &seeds.join("posting_rules.toml"),
            "[[posting_rules]]\nfact_kind = \"x\"\n",
        );
        write_file(
            &seeds.join("fact_projection_rules.toml"),
            "[[projections]]\nevent_kind = \"x\"\n",
        );
        let r = check(&dir);
        let row = status_of(&r, "seeds/posting_rules.toml").unwrap();
        assert_eq!(row.status, Status::Invalid, "{row:?}");
        assert!(row.detail.contains("[[posting_rule]]"), "{row:?}");
        let row = status_of(&r, "seeds/fact_projection_rules.toml").unwrap();
        assert_eq!(row.status, Status::Invalid, "{row:?}");
        assert!(row.detail.contains("[[projection]]"), "{row:?}");
    }

    /// The chart of accounts (backlog 41af5195) is judged by the
    /// ledger door's own validation: the real tenant's chart is OK and
    /// the detail names its rows; a kind outside the table's enum, a
    /// code declared twice and a parent not declared before its child
    /// are INVALID by row, in the door's words; rows under the wrong
    /// table name parse to nothing and are refused rather than passed.
    #[test]
    fn a_chart_of_accounts_is_judged_by_the_ledger_doors_own_validation() {
        let dir = scratch_dir("boss-cli-tenant-check-chart");
        write_file(&dir.join("tenant.toml"), "[meta]\ntenant_id = \"t\"\n");
        let seeds = dir.join("seeds");
        boss_testing::scratch::create_dir(&seeds);
        write_file(&seeds.join("workflows.toml"), "");
        write_file(
            &seeds.join("chart_of_accounts.toml"),
            "[[account]]\ncode = \"1000\"\nname = \"Bank\"\nkind = \"asset\"\nnormal_balance = \"debit\"\n\
             [[account]]\ncode = \"1010\"\nname = \"Stripe balance\"\nkind = \"asset\"\n\
             normal_balance = \"debit\"\nparent = \"1000\"\n\
             [[account]]\ncode = \"4100\"\nname = \"Sponsorship revenue\"\nkind = \"revenue\"\n\
             normal_balance = \"credit\"\n",
        );
        let r = check(&dir);
        let row = status_of(&r, "seeds/chart_of_accounts.toml").unwrap();
        assert_eq!(row.status, Status::Ok, "{row:?}");
        assert!(
            row.detail.starts_with("3 accounts")
                && row
                    .detail
                    .contains("1010 Stripe balance (asset/debit, under 1000)")
                && row
                    .detail
                    .contains("4100 Sponsorship revenue (revenue/credit)"),
            "{row:?}"
        );

        write_file(
            &seeds.join("chart_of_accounts.toml"),
            "[[account]]\ncode = \"6200\"\nname = \"Infrastructure\"\nkind = \"cost\"\nnormal_balance = \"debit\"\n",
        );
        let row = check(&dir);
        let row = status_of(&row, "seeds/chart_of_accounts.toml").unwrap();
        assert_eq!(row.status, Status::Invalid, "{row:?}");
        assert!(
            row.detail.contains("account #1 (6200)") && row.detail.contains("cost"),
            "{row:?}"
        );

        write_file(
            &seeds.join("chart_of_accounts.toml"),
            "[[account]]\ncode = \"1010\"\nname = \"Stripe balance\"\nkind = \"asset\"\n\
             normal_balance = \"debit\"\nparent = \"1000\"\n",
        );
        let row = check(&dir);
        let row = status_of(&row, "seeds/chart_of_accounts.toml").unwrap();
        assert_eq!(row.status, Status::Invalid, "{row:?}");
        assert!(
            row.detail.contains("parent `1000`") && row.detail.contains("earlier"),
            "{row:?}"
        );

        write_file(
            &seeds.join("chart_of_accounts.toml"),
            "[[accounts]]\ncode = \"x\"\n",
        );
        let row = check(&dir);
        let row = status_of(&row, "seeds/chart_of_accounts.toml").unwrap();
        assert_eq!(row.status, Status::Invalid, "{row:?}");
        assert!(row.detail.contains("[[account]]"), "{row:?}");
    }

    /// A RULE DECLARATION IS JUDGED BY THE DISPATCHER'S OWN READER
    /// (backlog 458971ef): the product's `[[rule]]` parser, the same
    /// `validate` the publish door applies, and the handler roster the
    /// product's own rules are held to (`cascade::handler_emits`). Each
    /// refusal names the rule and the field, in the loader's words;
    /// the rows under a wrong table name parse to nothing and are
    /// refused rather than passed.
    #[test]
    fn a_bad_rule_row_is_invalid_by_name() {
        let dir = scratch_dir("boss-cli-tenant-check-rules");
        write_file(&dir.join("tenant.toml"), "[meta]\ntenant_id = \"t\"\n");
        let seeds = dir.join("seeds");
        boss_testing::scratch::create_dir(&seeds);
        write_file(&seeds.join("workflows.toml"), "");
        let rules = |body: &str| {
            write_file(&seeds.join("rules.toml"), body);
            let r = check(&dir);
            status_of(&r, "seeds/rules.toml").unwrap().clone()
        };

        // A handler no build of BOSS has. (This row first named
        // jobs.complete_step_matching, "the sibling car's handler,
        // not yet landed" — it landed on #423 while this car waited,
        // and the row read OK: a fixture must not depend on what a
        // sibling car has not yet done.)
        let row = rules(
            "[[rule]]\nname = \"site-live\"\nwhy = \"\"\"\na reactor\n\"\"\"\n\
             on_event = \"jobs.job.closed\"\n[[rule.do]]\nhandler = \"jobs.no_such_handler\"\n",
        );
        assert_eq!(row.status, Status::Invalid, "{row:?}");
        assert!(
            row.detail.contains("site-live") && row.detail.contains("jobs.no_such_handler"),
            "{row:?}"
        );
        assert!(row.detail.contains("handler_emits"), "{row:?}");

        // No why: the product's own guard, in its words.
        let row = rules(
            "[[rule]]\nname = \"mute\"\non_event = \"jobs.job.closed\"\n\
             [[rule.do]]\nhandler = \"messages.notify\"\n",
        );
        assert_eq!(row.status, Status::Invalid, "{row:?}");
        assert!(
            row.detail.contains("`mute`") && row.detail.contains("why"),
            "{row:?}"
        );

        // A predicate that will not parse: the publish door's own gate.
        let row = rules(
            "[[rule]]\nname = \"broken\"\nwhy = \"\"\"\na reactor\n\"\"\"\n\
             on_event = \"jobs.job.closed\"\nwhen = \"kind = (\"\n\
             [[rule.do]]\nhandler = \"messages.notify\"\n",
        );
        assert_eq!(row.status, Status::Invalid, "{row:?}");
        assert!(row.detail.contains("broken"), "{row:?}");

        // The wrong table name parses to zero rules; refused, naming both.
        let row = rules("[[rules]]\nname = \"x\"\n");
        assert_eq!(row.status, Status::Invalid, "{row:?}");
        assert!(row.detail.contains("[[rule]]"), "{row:?}");

        // Empty is a tenant with no reactors, not a defect.
        let row = rules("# none yet\n");
        assert_eq!(row.status, Status::Ok, "{row:?}");
        assert!(row.detail.contains("0 rules"), "{row:?}");
    }

    #[test]
    fn a_missing_required_file_fails_and_an_absent_optional_one_is_silent() {
        let dir = scratch_dir("boss-cli-tenant-check-missing");
        write_file(&dir.join("tenant.toml"), "[meta]\ntenant_id = \"t\"\n");
        let r = check(&dir);
        let wf = status_of(&r, "seeds/workflows.toml").unwrap();
        assert_eq!(wf.status, Status::Missing, "{wf:?}");
        assert!(wf.detail.contains("required"), "{wf:?}");
        assert!(status_of(&r, "seeds/policy_rules.toml").is_none());
        assert!(!r.passed());
    }

    #[test]
    fn the_manifest_is_accepted_at_either_spelling_and_needs_a_tenant_id() {
        // N-1 spelling: the examples keep it under seeds/ because the
        // deployment's BOSS_TENANT_MANIFEST_TOML points there (car 2).
        let dir = scratch_dir("boss-cli-tenant-check-manifest-under-seeds");
        put(&dir, "seeds/tenant.toml", "[meta]\ntenant_id = \"t\"\n");
        put(&dir, "seeds/workflows.toml", "");
        let r = check(&dir);
        assert_eq!(
            status_of(&r, "seeds/tenant.toml").unwrap().status,
            Status::Ok
        );
        assert!(status_of(&r, "tenant.toml").is_none());

        let dir = scratch_dir("boss-cli-tenant-check-manifest-no-id");
        write_file(&dir.join("tenant.toml"), "[modules]\nledger = true\n");
        let r = check(&dir);
        let m = status_of(&r, "tenant.toml").unwrap();
        assert_eq!(m.status, Status::Invalid, "{m:?}");
        assert!(m.detail.contains("tenant_id"), "{m:?}");
    }

    #[test]
    fn an_invalid_workflow_reports_the_loaders_own_error() {
        let dir = scratch_dir("boss-cli-tenant-check-bad-workflow");
        write_file(&dir.join("tenant.toml"), "[meta]\ntenant_id = \"t\"\n");
        put(
            &dir,
            "seeds/workflows.toml",
            "[[workflow]]\nkind = \"x\"\nlabel = \"X\"\ncategory = \"c\"\nsubject_kinds = [\"custom\"]\n\
             [[workflow.step]]\ntitle = \"a\"\nkind = \"task\"\nready_when = \"steps.nope.done\"\n",
        );
        let r = check(&dir);
        let wf = status_of(&r, "seeds/workflows.toml").unwrap();
        assert_eq!(wf.status, Status::Invalid, "{wf:?}");
        assert!(!wf.detail.is_empty());
    }

    /// CLAUDE.md §9a: the doc's table and the code's contract are one
    /// fact in two places, so the doc carries the rendered table
    /// between two markers and this test holds them equal.
    #[test]
    fn the_contract_doc_carries_the_codes_table() {
        let doc =
            std::fs::read_to_string(boss_testing::repo_root().join("docs/tenant-contract.md"))
                .expect("docs/tenant-contract.md exists");
        let begin = doc
            .find(TABLE_BEGIN)
            .expect("doc has the contract-table begin marker");
        let end = doc.find(TABLE_END).expect("doc has the end marker");
        let in_doc = doc[begin + TABLE_BEGIN.len()..end].trim();
        assert_eq!(
            in_doc,
            contract_table().trim(),
            "docs/tenant-contract.md's table drifted from CONTRACT in tenant.rs; \
             paste `boss tenant contract` output between the markers"
        );
    }

    #[test]
    fn every_contract_path_is_under_seeds_or_the_manifest() {
        for e in CONTRACT {
            assert!(!e.paths.is_empty());
            for p in e.paths {
                assert!(
                    p.starts_with("seeds/") || *p == "tenant.toml",
                    "{p}: the contract is tenant.toml + seeds/"
                );
            }
        }
        assert!(CONTRACT.iter().any(|e| e.paths[0] == "tenant.toml"));
        assert!(
            CONTRACT
                .iter()
                .any(|e| e.paths[0] == "seeds/workflows.toml")
        );
    }
}
