//! `boss tenant export <dir> [--gateway <url> | --door <url>]
//! [--tenant <id>]` — the live registries written back into the
//! contract's file shape (design e187198f car 3, backlog e618f3ac).
//!
//! WHERE IT CAN BE RUN FROM (backlog ea3c8234, car 4's first half).
//! The reads are made as the seed identity, in the `x-boss-user`
//! header, which only a service's own port accepts: the gateway signs
//! that header from a SESSION and answers a sessionless reader 401, so
//! `--gateway` is an in-cluster convenience and not a route for the
//! machine that is supposed to commit the export. `--door` is that
//! route — the LAN machine door (`boss-jobs-internal`, the address
//! `infra/estate/estate.toml` spells as the system of record), one
//! host with each service on its `boss_ports` port, which the
//! forge and the dev pod can both reach. Measured 2026-09-22 before
//! this car: the door carried classes and locations but NOT policy,
//! the calendar or subject-kinds, so an off-cluster export could
//! read some of the instance and not the rest — and a partial snapshot
//! rendered into the tenant repo would DELETE rows from the files it
//! rewrites. The three ports land with this car.
//!
//! WHY. The instance is the truth (cars 1 and 2: every door is
//! insert-if-absent, the launcher publishes once per database). That
//! decision only closes if the truth can flow BACK: an operator's edit
//! in `/it/registry`, a calendar closed for a holiday, an agent's new
//! alias — each lives in the instance and nowhere else until something
//! writes it into the tenant repo. Until 2026-09-18 nothing did
//! (tenant.rs: init | check | contract | publish | published), so the
//! repo drifted from the instance one edit at a time and the next
//! bootstrap from it would have been a lie.
//!
//! THE INVERSE OF `check`'S LOADERS. Each file is written in the shape
//! the CONTRACT names for it — the shape `boss_ledger::chart::
//! load_chart_toml`, `boss_jobs::agents::load_agents_toml`,
//! `boss_dispatcher::rules::registry::parse_raw_file` and the rest
//! READ — so that `check` on the exported directory is OK on every
//! row and a publish of it back into the same instance writes nothing
//! (the round-trip pin in the tests). The rows are read through the
//! same doors `publish` and `check` already use, signed as the seed
//! identity (reads of the agents, sensors and credentials registries
//! and of the Workflows are tier-gated).
//!
//! DETERMINISTIC, SO A RE-EXPORT IS A NO-OP DIFF. Rows sorted by their
//! key, keys sorted within a row, one formatting (the `toml` crate's
//! pretty form; `serde_json`'s pretty form with sorted keys), nulls
//! and the loaders' defaults omitted. Exporting an unchanged instance
//! into a directory it already exported to changes no file, and the
//! verb prints `added` / `changed` / `unchanged` per file rather than
//! writing silently. Two files are NOT rewritten wholesale, because
//! the instance does not hold everything they say:
//!
//! - `tenant.toml` carries `[modules]`, `[labels]` and whatever else
//!   the deployment reads from the FILE (no registry holds them), so
//!   only its `display_name` line is edited, to the company Subject's
//!   label; an absent manifest is written minimal.
//! - `seeds/rules.toml`'s `why` is authoring metadata the
//!   `dispatcher_rules` table deliberately does not carry
//!   (`registry::RawRule::why` is read from the file and never
//!   serialized), so each rule's `why` is kept from the file the
//!   export overwrites; a rule the file did not record gets
//!   a `why` that SAYS it was exported without one, which `check`
//!   accepts and a reader cannot mistake for a justification.
//!
//! WHOSE ROWS. A registry that marks ownership is exported for the
//! tenant only: Workflows by `owning_team`, dispatcher rules and
//! posting rules by `source = tenant:<id>`, sensors by `tenant_id`.
//! People and agents are the tenant's roster whole. The registries
//! with NO ownership column — classes, locations, the chart, the tax
//! regime, calendars, policy, credentials, projections — are exported
//! whole too, platform-seeded rows included: the instance is the
//! truth, and a publish of the file back finds every one of them
//! `already as declared`. Retired classes and locations are left out.
//!
//! A registry with NO live rows leaves an existing file alone when the
//! file declares nothing (the scaffold's comment-only `tax.toml` is
//! that), rewrites it empty when it declares rows the instance no
//! longer holds, and writes nothing when there is no file. Nothing is
//! ever deleted: a tenant engine's files (`accounts.toml`,
//! `vendors.toml`) are not the export's to touch.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::Path;

use anyhow::{Context, Result, bail};
use boss_core::tenant_manifest::TenantToml;
use reqwest::blocking::Client;
use serde_json::{Value, json};

use crate::tenant_publish::{Bases, SEED_USER};

/// The instance's registries, as the doors answer them. Pure data: the
/// renderer below is a function of this and the directory, and the
/// tests build one by hand.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Snapshot {
    pub tenant_id: String,
    /// The company Subject's label (`GET /api/subjects/company`, the
    /// row whose id is the tenant id), or `None` when not minted.
    pub company_label: Option<String>,
    /// `GET /api/classes?subject_kind=<k>` over every registered kind;
    /// retired rows dropped.
    pub classes: Vec<Value>,
    /// `GET /api/ledger/accounts` — with the parent's CODE.
    pub accounts: Vec<Value>,
    pub tax_kinds: Vec<Value>,
    pub sales_tax_rates: Vec<Value>,
    /// The location tree, walked root-down through `?parent_id=`.
    pub locations: Vec<Value>,
    pub calendars: Vec<Value>,
    /// `GET /api/policy/rules`, active rows.
    pub policy: Vec<Value>,
    pub people: Vec<Value>,
    pub agents: Vec<Value>,
    /// `GET /api/workflows`, the tenant's `owning_team` only.
    pub workflows: Vec<Value>,
    pub credentials: Vec<Value>,
    /// `GET /api/sensors`, the tenant's `tenant_id` only.
    pub sensors: Vec<Value>,
    /// `GET /api/ledger/posting-rules`, `source = tenant:<id>` only.
    pub posting_rules: Vec<Value>,
    pub projections: Vec<Value>,
    /// `GET /api/dispatcher/rules`, `source = tenant:<id>` only.
    pub rules: Vec<Value>,
}

// ---------------------------------------------------------------------------
// Reading the instance
// ---------------------------------------------------------------------------

fn seed_client() -> Result<Client> {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        "x-boss-user",
        reqwest::header::HeaderValue::from_static(SEED_USER),
    );
    Ok(Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .default_headers(headers)
        .build()?)
}

fn get(client: &Client, base: &str, path: &str) -> Result<Value> {
    let u = format!("{}{path}", base.trim_end_matches('/'));
    let resp = client.get(&u).send().with_context(|| format!("GET {u}"))?;
    let status = resp.status();
    if !status.is_success() {
        bail!("GET {u} → {status} {}", resp.text().unwrap_or_default());
    }
    resp.json()
        .with_context(|| format!("GET {u}: the body did not parse as JSON"))
}

/// A list door's rows — a bare array, `{data}`, or the dispatcher's
/// `{rules}` — or a refusal.
///
/// It used to answer an empty list for anything else (backlog
/// 10776b6c), and the dispatcher answers a failed rule load with a 200
/// carrying `"error"` beside `"rules": []`, so an export read at that
/// moment wrote a tenant with no rules. An envelope that names an
/// `error` refuses with it; every other shape is decided by the one
/// rows helper, which refuses a body that is not a list (7b7e0529) —
/// and, through `every_row`, one whose `total` counts more rows than it
/// carries (6cf47547): the export rewrites the repo's files from these
/// rows, so a page read as the registry would drop the rest from them.
fn rows_of(v: Value) -> Result<Vec<Value>> {
    let v = match v {
        Value::Object(mut o) => {
            if let Some(e) = o.get("error").filter(|e| !e.is_null()) {
                bail!("the registry answered an error, so its rows cannot be read as zero: {e}");
            }
            match o.remove("rules") {
                Some(rules) if !o.contains_key("data") => rules,
                _ => Value::Object(o),
            }
        }
        other => other,
    };
    crate::train::every_row(Some(v))
}

/// One list door read, its rows refused rather than guessed, and the
/// refusal naming the door that answered.
fn list(client: &Client, base: &str, path: &str) -> Result<Vec<Value>> {
    rows_of(get(client, base, path)?).with_context(|| format!("GET {base}{path}"))
}

fn str_of<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}

/// Read every registry through the doors. Blocking HTTP — call from
/// `spawn_blocking`.
pub fn read_instance(bases: &Bases, tenant_id: &str) -> Result<Snapshot> {
    let client = seed_client()?;
    let source = format!("tenant:{tenant_id}");

    let kinds = list(&client, &bases.subjects, "/api/subject-kinds")?;
    let mut classes = Vec::new();
    for k in kinds.iter().map(|k| str_of(k, "kind").to_string()) {
        let path = format!("/api/classes?subject_kind={k}");
        classes.extend(
            list(&client, &bases.classes, &path)?
                .into_iter()
                .filter(|c| c.get("retired_at").is_none_or(Value::is_null)),
        );
    }

    // The location tree: roots, then each row's children, breadth
    // first — the list answers one level at a time.
    let mut locations = Vec::new();
    let mut queue: VecDeque<Option<String>> = VecDeque::from([None]);
    while let Some(parent) = queue.pop_front() {
        let path = match &parent {
            None => "/api/locations".to_string(),
            Some(id) => format!("/api/locations?parent_id={id}"),
        };
        for row in list(&client, &bases.locations, &path)? {
            if row.get("retired_at").is_some_and(|r| !r.is_null()) {
                continue;
            }
            queue.push_back(Some(str_of(&row, "id").to_string()));
            locations.push(row);
        }
    }

    let company_label = list(&client, &bases.subjects, "/api/subjects/company")?
        .into_iter()
        .find(|r| str_of(r, "id") == tenant_id)
        .and_then(|r| r.get("label").and_then(Value::as_str).map(str::to_string));

    Ok(Snapshot {
        tenant_id: tenant_id.to_string(),
        company_label,
        classes,
        accounts: list(&client, &bases.ledger, "/api/ledger/accounts")?,
        tax_kinds: list(&client, &bases.ledger, "/api/ledger/tax-kinds")?,
        sales_tax_rates: list(&client, &bases.ledger, "/api/ledger/sales-tax-rates")?,
        locations,
        calendars: list(&client, &bases.calendar, "/api/calendar/business-calendars")?,
        policy: list(&client, &bases.policy, "/api/policy/rules")?
            .into_iter()
            .filter(|r| r.get("active").and_then(Value::as_bool).unwrap_or(true))
            .collect(),
        people: list(&client, &bases.people, "/api/people")?,
        agents: list(&client, &bases.jobs, "/api/agents")?,
        workflows: list(&client, &bases.jobs, "/api/workflows")?
            .into_iter()
            .filter(|w| str_of(w, "owning_team") == tenant_id)
            .collect(),
        credentials: list(&client, &bases.jobs, "/api/credentials")?,
        sensors: list(&client, &bases.jobs, "/api/sensors")?
            .into_iter()
            .filter(|s| str_of(s, "tenant_id") == tenant_id)
            .collect(),
        posting_rules: list(&client, &bases.ledger, "/api/ledger/posting-rules")?
            .into_iter()
            .filter(|r| str_of(r, "source") == source)
            .collect(),
        projections: list(&client, &bases.ledger, "/api/ledger/fact-projection-rules")?,
        rules: list(&client, &bases.dispatcher, "/api/dispatcher/rules")?
            .into_iter()
            .filter(|r| str_of(r, "source") == source)
            .collect(),
    })
}

// ---------------------------------------------------------------------------
// Rendering — pure over the snapshot and the directory's current files
// ---------------------------------------------------------------------------

/// One file the export produces: its contract path, its text, and
/// how many rows it declares (0 = the empty form).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rendered {
    pub path: String,
    pub text: String,
    pub rows: usize,
}

/// A JSON value with object keys sorted at every depth and nulls
/// dropped — what both file formats are written from.
fn canonical(v: &Value) -> Value {
    match v {
        Value::Object(o) => Value::Object(
            o.iter()
                .filter(|(_, v)| !v.is_null())
                .map(|(k, v)| (k.clone(), canonical(v)))
                .collect::<serde_json::Map<_, _>>(),
        ),
        Value::Array(a) => Value::Array(a.iter().map(canonical).collect()),
        other => other.clone(),
    }
}

/// The keys of `row` to keep: the loader's own, in the loader's
/// shape. Everything else the door answered with (timestamps,
/// book-keeping, a source label) is the instance's, not the file's.
fn pick(row: &Value, keys: &[&str]) -> Value {
    Value::Object(
        keys.iter()
            .filter_map(|k| row.get(*k).map(|v| ((*k).to_string(), v.clone())))
            .collect(),
    )
}

fn drop_if(row: &mut Value, key: &str, default: &Value) {
    if let Some(o) = row.as_object_mut()
        && o.get(key) == Some(default)
    {
        o.remove(key);
    }
}

/// A TOML document of one array-of-tables: `[[table]]` per row, the
/// header naming where the rows came from.
fn toml_doc(table: &str, rows: &[Value]) -> Result<String> {
    let mut doc = toml::Table::new();
    let items = rows
        .iter()
        .map(|r| toml::Value::try_from(canonical(r)))
        .collect::<Result<Vec<_>, _>>()
        .with_context(|| format!("[[{table}]]: a row does not fit TOML"))?;
    doc.insert(table.to_string(), toml::Value::Array(items));
    let body = toml::to_string_pretty(&doc)?;
    Ok(format!("{}{body}", header(table)))
}

fn header(table: &str) -> String {
    format!(
        "# [[{table}]] rows exported from the instance by `boss tenant export`\n\
         # (design e187198f: the instance is the truth; this file is its\n\
         # bootstrap). Edit the instance, then export — not this file.\n\n"
    )
}

fn json_doc(rows: &[Value]) -> Result<String> {
    let v = Value::Array(rows.iter().map(canonical).collect());
    Ok(format!("{}\n", serde_json::to_string_pretty(&v)?))
}

fn sorted_by(rows: &[Value], key: impl Fn(&Value) -> String) -> Vec<Value> {
    let mut out: Vec<Value> = rows.to_vec();
    out.sort_by_cached_key(key);
    out
}

/// Rows ordered parent-first: a chart account's `parent` and a
/// location's `parent_id` must name a row declared EARLIER in the
/// file (the chart loader refuses otherwise), so each row is placed
/// at its depth in the tree, then by its key.
fn parent_first(rows: &[Value], id_key: &str, parent_key: &str) -> Vec<Value> {
    let parents: BTreeMap<String, Option<String>> = rows
        .iter()
        .map(|r| {
            (
                str_of(r, id_key).to_string(),
                r.get(parent_key)
                    .and_then(Value::as_str)
                    .map(str::to_string),
            )
        })
        .collect();
    let depth = |id: &str| {
        let mut d = 0usize;
        let mut cur = parents.get(id).cloned().flatten();
        while let Some(p) = cur {
            d += 1;
            // A cycle cannot come from the registry (the FK forbids it);
            // the bound only keeps a malformed answer from looping.
            if d > rows.len() {
                break;
            }
            cur = parents.get(&p).cloned().flatten();
        }
        d
    };
    let mut out: Vec<Value> = rows.to_vec();
    out.sort_by_cached_key(|r| (depth(str_of(r, id_key)), str_of(r, id_key).to_string()));
    out
}

fn render_workflow(w: &Value) -> Value {
    let mut row = pick(
        w,
        &[
            "kind",
            "label",
            "category",
            "subject_kinds",
            "description",
            "metadata",
            "metadata_schema",
            "entitlements",
        ],
    );
    for k in ["metadata", "metadata_schema", "entitlements"] {
        drop_if(&mut row, k, &json!({}));
    }
    let steps: Vec<Value> = w
        .get("steps")
        .and_then(Value::as_array)
        .map(|s| s.iter().map(render_step).collect())
        .unwrap_or_default();
    row["step"] = Value::Array(steps);
    row
}

/// A step in the seed loader's `[[workflow.step]]` shape: the spec's
/// own keys, minus what the loader defaults — and minus an
/// `authority_role` the loader derives from the audience itself
/// (`workflow_toml_to_spec` writes it from `selectors_for`), so the
/// file declares who the step is for ONCE, as the author did.
fn render_step(s: &Value) -> Value {
    let mut row = pick(
        s,
        &[
            "title",
            "kind",
            "ready_when",
            "terminal",
            "title_template",
            "sign_offs_required",
            "assurance_required",
            "duration_hours",
            "labor_hours",
            "wall_clock_hours",
            "fields",
            "authority_role",
            "claimable",
            "audience",
            "agent",
            "metadata_defaults",
        ],
    );
    drop_if(&mut row, "title_template", &json!(""));
    drop_if(&mut row, "sign_offs_required", &json!([]));
    drop_if(&mut row, "fields", &json!([]));
    drop_if(&mut row, "metadata_defaults", &json!({}));
    let derived = s
        .get("audience")
        .cloned()
        .and_then(|a| serde_json::from_value::<boss_jobs::audience::Audience>(a).ok())
        .and_then(|a| boss_jobs::audience::selectors_for(&a).authority_role);
    if let Some(derived) = derived
        && s.get("authority_role").and_then(Value::as_str) == Some(derived.as_str())
    {
        row.as_object_mut().map(|o| o.remove("authority_role"));
    }
    row
}

/// The policy file's `[[grants]]` shape is a product (roles x
/// resources x actions) under one scope; the inverse groups the live
/// rules by (role, scope, resource) and lists the actions, which is
/// always a product and always the same text for the same rules.
fn render_grants(rules: &[Value]) -> Vec<Value> {
    let mut groups: BTreeMap<(String, String, String), BTreeSet<String>> = BTreeMap::new();
    for r in rules {
        let scope = match r.get("scope") {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Object(o)) => o
                .iter()
                .next()
                .map(|(k, v)| format!("{k}:{}", v.as_str().unwrap_or("")))
                .unwrap_or_default(),
            _ => continue,
        };
        groups
            .entry((
                str_of(r, "role").to_string(),
                scope,
                str_of(r, "resource").to_string(),
            ))
            .or_default()
            .insert(str_of(r, "action").to_string());
    }
    groups
        .into_iter()
        .map(|((role, scope, resource), actions)| {
            json!({
                "role": role,
                "resource": resource,
                "actions": actions.into_iter().collect::<Vec<_>>(),
                "scope": scope,
            })
        })
        .collect()
}

/// `tenant.toml` with its `display_name` line set to the company's
/// label — the one fact of the manifest the instance holds. Every
/// other line is kept as written (and returned as is when the label
/// already matches, so the file is reported unchanged rather than
/// omitted); an absent manifest is written minimal.
fn render_manifest(existing: Option<&str>, tenant_id: &str, label: Option<&str>) -> String {
    let Some(text) = existing else {
        let mut out = format!("[meta]\ntenant_id = \"{tenant_id}\"\n");
        if let Some(l) = label {
            out.push_str(&format!("display_name = {}\n", toml_str(l)));
        }
        return out;
    };
    let Some(label) = label else {
        return text.to_string();
    };
    let current = TenantToml::parse(text)
        .ok()
        .and_then(|t| t.meta.display_name);
    if current.as_deref() == Some(label) {
        return text.to_string();
    }
    let mut out = Vec::new();
    let mut in_meta = false;
    let mut replaced = false;
    for line in text.lines() {
        let t = line.trim();
        if t.starts_with('[') {
            if in_meta && !replaced {
                out.push(format!("display_name = {}", toml_str(label)));
                replaced = true;
            }
            in_meta = t == "[meta]";
        } else if in_meta
            && !replaced
            && t.split('=')
                .next()
                .map(str::trim)
                .is_some_and(|k| k == "display_name")
        {
            out.push(format!("display_name = {}", toml_str(label)));
            replaced = true;
            continue;
        }
        out.push(line.to_string());
    }
    if !replaced {
        out.push(format!("display_name = {}", toml_str(label)));
    }
    format!("{}\n", out.join("\n"))
}

fn toml_str(s: &str) -> String {
    toml::Value::String(s.to_string()).to_string()
}

/// The `why` each rule in the directory's `seeds/rules.toml` records,
/// read by the dispatcher's own reader (`registry::why_in`) — one
/// definition of "does this rule say why", CLAUDE.md §9a.
fn whys_in(existing: Option<&str>, names: &[String]) -> BTreeMap<String, String> {
    let Some(src) = existing else {
        return BTreeMap::new();
    };
    names
        .iter()
        .filter_map(|n| {
            boss_dispatcher::rules::registry::why_in(src, n)
                .ok()
                .flatten()
                .map(|w| (n.clone(), w))
        })
        .collect()
}

/// A `why` for a rule the file never recorded one for: it says so.
/// `check` accepts any non-empty `why`; this one cannot be read as a
/// justification, which is the point.
pub const EXPORTED_WITHOUT_WHY: &str = "Exported from the instance by `boss tenant export` \
     without a recorded why: the registry does not carry one. Say here which standing \
     exemption this reaction claims — timer, external glue, or cross-protocol reactor.";

fn render_rule(r: &Value, whys: &BTreeMap<String, String>) -> Value {
    let name = str_of(r, "name").to_string();
    let mut row = json!({
        "name": name,
        "why": whys.get(&name).cloned().unwrap_or_else(|| EXPORTED_WITHOUT_WHY.to_string()),
    });
    for k in ["version", "on_event", "schedule", "when", "delay"] {
        if let Some(v) = r.get(k).filter(|v| !v.is_null()) {
            row[k] = v.clone();
        }
    }
    row["do"] = r.get("do").cloned().unwrap_or(json!([]));
    row
}

/// The directory's current text of `rel`, when present.
fn existing(dir: &Path, rel: &str) -> Option<String> {
    std::fs::read_to_string(dir.join(rel)).ok()
}

/// One registry's file: rows in the file's shape, as a TOML
/// array-of-tables or (with no table name) a JSON array.
fn file(out: &mut Vec<Rendered>, path: &str, rows: Vec<Value>, table: Option<&str>) -> Result<()> {
    let text = match table {
        Some(t) => toml_doc(t, &rows)?,
        None => json_doc(&rows)?,
    };
    out.push(Rendered {
        path: path.to_string(),
        text,
        rows: rows.len(),
    });
    Ok(())
}

/// Every file the export writes for `snap`, in the contract's order.
/// Pure: reads the directory (the manifest's other lines, the rules'
/// whys, which files exist) and writes nothing.
pub fn render(snap: &Snapshot, dir: &Path) -> Result<Vec<Rendered>> {
    let mut out = Vec::new();
    let manifest_rel = ["tenant.toml", "seeds/tenant.toml"]
        .into_iter()
        .find(|p| dir.join(p).is_file())
        .unwrap_or("tenant.toml");
    out.push(Rendered {
        path: manifest_rel.to_string(),
        text: render_manifest(
            existing(dir, manifest_rel).as_deref(),
            &snap.tenant_id,
            snap.company_label.as_deref(),
        ),
        rows: 1,
    });

    let workflows: Vec<Value> = sorted_by(&snap.workflows, |w| str_of(w, "kind").to_string())
        .iter()
        .map(render_workflow)
        .collect();
    file(
        &mut out,
        "seeds/workflows.toml",
        workflows,
        Some("workflow"),
    )?;

    file(
        &mut out,
        "seeds/policy_rules.toml",
        render_grants(&snap.policy),
        Some("grants"),
    )?;

    let classes = sorted_by(&snap.classes, |c| {
        format!("{}/{}", str_of(c, "subject_kind"), str_of(c, "code"))
    })
    .iter()
    .map(|c| {
        let mut row = pick(
            c,
            &[
                "subject_kind",
                "code",
                "display_name",
                "parent_code",
                "member_attribute",
                "metadata",
                "sort_order",
            ],
        );
        drop_if(&mut row, "metadata", &json!({}));
        row
    })
    .collect();
    file(&mut out, "seeds/classes.json", classes, None)?;

    let accounts = parent_first(&snap.accounts, "code", "parent")
        .iter()
        .map(|a| {
            let mut row = pick(a, &["code", "name", "kind", "parent"]);
            row["normal_balance"] = a
                .get("normal_balance")
                .or_else(|| a.get("normal_side"))
                .cloned()
                .unwrap_or(Value::Null);
            row
        })
        .collect();
    file(
        &mut out,
        "seeds/chart_of_accounts.toml",
        accounts,
        Some("account"),
    )?;

    // The tax regime is two tables in one file.
    {
        let kinds = sorted_by(&snap.tax_kinds, |k| str_of(k, "kind").to_string())
            .iter()
            .map(|k| {
                pick(
                    k,
                    &[
                        "kind",
                        "liability_account",
                        "expense_account",
                        "derive_basis",
                    ],
                )
            })
            .collect::<Vec<_>>();
        let rates = sorted_by(&snap.sales_tax_rates, |r| str_of(r, "state").to_string())
            .iter()
            .map(|r| pick(r, &["state", "jurisdiction", "rate_bps"]))
            .collect::<Vec<_>>();
        let mut doc = toml::Table::new();
        let to_toml = |rows: &[Value]| {
            rows.iter()
                .map(|r| toml::Value::try_from(canonical(r)))
                .collect::<Result<Vec<_>, _>>()
        };
        doc.insert("tax_kind".into(), toml::Value::Array(to_toml(&kinds)?));
        doc.insert(
            "sales_tax_rate".into(),
            toml::Value::Array(to_toml(&rates)?),
        );
        let body = toml::to_string_pretty(&doc)?;
        out.push(Rendered {
            path: "seeds/tax.toml".to_string(),
            text: format!("{}{body}", header("tax_kind]] + [[sales_tax_rate")),
            rows: kinds.len() + rates.len(),
        });
    }

    let people = sorted_by(&snap.people, |p| str_of(p, "id").to_string());
    file(&mut out, "seeds/employees.json", people, None)?;

    let calendars = sorted_by(&snap.calendars, |c| str_of(c, "code").to_string())
        .iter()
        .map(|c| pick(c, &["code", "name", "weekend", "closed"]))
        .collect();
    file(&mut out, "seeds/business_calendars.json", calendars, None)?;

    let credentials = sorted_by(&snap.credentials, |c| str_of(c, "id").to_string())
        .iter()
        .map(|c| {
            let mut row = pick(
                c,
                &[
                    "id",
                    "kind",
                    "issuer",
                    "principal",
                    "scopes",
                    "storage_location",
                    "consumers",
                    "rotation_policy",
                    "notes",
                ],
            );
            drop_if(&mut row, "scopes", &json!([]));
            drop_if(&mut row, "consumers", &json!([]));
            drop_if(&mut row, "notes", &json!(""));
            row
        })
        .collect();
    file(
        &mut out,
        "seeds/credentials.toml",
        credentials,
        Some("credential"),
    )?;

    let sensors = sorted_by(&snap.sensors, |s| str_of(s, "id").to_string())
        .iter()
        .map(|s| {
            let mut row = pick(
                s,
                &[
                    "id",
                    "source",
                    "credential",
                    "every_minutes",
                    "subject_kind",
                    "enabled",
                ],
            );
            row["opens"] = s
                .get("opens")
                .or_else(|| s.get("opens_kind"))
                .cloned()
                .unwrap_or(Value::Null);
            // A push-only source polls nothing: the loader's defaults.
            drop_if(&mut row, "credential", &json!(""));
            drop_if(&mut row, "every_minutes", &json!(0));
            drop_if(&mut row, "enabled", &json!(true));
            row
        })
        .collect();
    file(&mut out, "seeds/sensors.toml", sensors, Some("sensor"))?;

    let agents = sorted_by(&snap.agents, |a| str_of(a, "id").to_string())
        .iter()
        .map(|a| {
            let mut row = pick(
                a,
                &[
                    "id",
                    "display_name",
                    "default_model",
                    "aliases",
                    "role",
                    "department",
                    "hourly_budget_usd_micros",
                    "max_concurrent_runs",
                ],
            );
            drop_if(&mut row, "aliases", &json!([]));
            row
        })
        .collect();
    file(&mut out, "seeds/agents.toml", agents, Some("agent"))?;

    let posting = sorted_by(&snap.posting_rules, |r| {
        format!(
            "{} {:010}",
            str_of(r, "fact_kind"),
            r.get("version").and_then(Value::as_u64).unwrap_or(1)
        )
    })
    .iter()
    .map(|r| pick(r, &["fact_kind", "version", "basis", "lines"]))
    .collect();
    file(
        &mut out,
        "seeds/posting_rules.toml",
        posting,
        Some("posting_rule"),
    )?;

    let projections = sorted_by(&snap.projections, |p| {
        format!(
            "{} {}",
            str_of(p, "event_kind"),
            p.get("when")
                .map(|w| canonical(w).to_string())
                .unwrap_or_default()
        )
    })
    .iter()
    .map(|p| {
        pick(
            p,
            &[
                "event_kind",
                "when",
                "fact_kind",
                "source_table",
                "source_id_path",
                "happened_on_path",
                "created_by_path",
            ],
        )
    })
    .collect();
    file(
        &mut out,
        "seeds/fact_projection_rules.toml",
        projections,
        Some("projection"),
    )?;

    let locations = parent_first(&snap.locations, "id", "parent_id")
        .iter()
        .map(|l| {
            let mut row = pick(
                l,
                &[
                    "id",
                    "name",
                    "kind",
                    "timezone",
                    "parent_id",
                    "latitude",
                    "longitude",
                    "address",
                    "account_id",
                    "metadata",
                ],
            );
            drop_if(&mut row, "metadata", &json!({}));
            row
        })
        .collect();
    file(
        &mut out,
        "seeds/locations.toml",
        locations,
        Some("location"),
    )?;

    let names: Vec<String> = snap
        .rules
        .iter()
        .map(|r| str_of(r, "name").to_string())
        .collect();
    let whys = whys_in(existing(dir, "seeds/rules.toml").as_deref(), &names);
    let rules = sorted_by(&snap.rules, |r| str_of(r, "name").to_string())
        .iter()
        .map(|r| render_rule(r, &whys))
        .collect();
    file(&mut out, "seeds/rules.toml", rules, Some("rule"))?;

    Ok(out)
}

// ---------------------------------------------------------------------------
// Writing — the diff against the directory, then the files
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Added,
    Changed,
    Unchanged,
    /// The instance holds no rows and the file declares none (or does
    /// not exist): left as it is, never written.
    Skipped,
}

impl Outcome {
    fn label(self) -> &'static str {
        match self {
            Outcome::Added => "added",
            Outcome::Changed => "changed",
            Outcome::Unchanged => "unchanged",
            Outcome::Skipped => "skipped",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub path: String,
    pub outcome: Outcome,
    pub rows: usize,
}

/// A file that declares nothing: absent, or every line blank or a
/// comment (TOML), or an empty JSON array. Such a file is left alone
/// when the instance holds no rows either — the scaffold's
/// comment-only files stay as the operator wrote them.
fn declares_nothing(text: Option<&str>) -> bool {
    match text {
        None => true,
        Some(t) => {
            let trimmed = t.trim();
            trimmed == "[]"
                || trimmed
                    .lines()
                    .all(|l| l.trim().is_empty() || l.trim_start().starts_with('#'))
        }
    }
}

/// What writing `rendered` into `dir` would change, per file.
pub fn diff(rendered: &[Rendered], dir: &Path) -> Vec<Change> {
    rendered
        .iter()
        .map(|r| {
            let current = existing(dir, &r.path);
            let outcome = if r.rows == 0 && declares_nothing(current.as_deref()) {
                Outcome::Skipped
            } else if current.is_none() {
                Outcome::Added
            } else if current.as_deref() == Some(r.text.as_str()) {
                Outcome::Unchanged
            } else {
                Outcome::Changed
            };
            Change {
                path: r.path.clone(),
                outcome,
                rows: r.rows,
            }
        })
        .collect()
}

/// Write the added and changed files; the rest are untouched.
pub fn write(rendered: &[Rendered], changes: &[Change], dir: &Path) -> Result<()> {
    for (r, c) in rendered.iter().zip(changes) {
        if !matches!(c.outcome, Outcome::Added | Outcome::Changed) {
            continue;
        }
        let path = dir.join(&r.path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create {}", parent.display()))?;
        }
        std::fs::write(&path, &r.text).with_context(|| format!("write {}", path.display()))?;
    }
    Ok(())
}

/// Render, diff and write `snap` into `dir`, returning the changes.
pub fn export_into(snap: &Snapshot, dir: &Path) -> Result<Vec<Change>> {
    let rendered = render(snap, dir)?;
    let changes = diff(&rendered, dir);
    write(&rendered, &changes, dir)?;
    Ok(changes)
}

pub fn render_changes(dir: &Path, tenant_id: &str, changes: &[Change]) -> String {
    let width = changes.iter().map(|c| c.path.len()).max().unwrap_or(0);
    let mut out = format!(
        "boss tenant export {} (tenant_id {tenant_id})\n",
        dir.display()
    );
    for c in changes {
        out.push_str(&format!(
            "  {:<9} {:<width$}  {} row(s)\n",
            c.outcome.label(),
            c.path,
            c.rows,
            width = width
        ));
    }
    let count = |o: Outcome| changes.iter().filter(|c| c.outcome == o).count();
    out.push_str(&format!(
        "{} added, {} changed, {} unchanged, {} skipped — the instance is the truth; \
         this directory is its bootstrap\n",
        count(Outcome::Added),
        count(Outcome::Changed),
        count(Outcome::Unchanged),
        count(Outcome::Skipped),
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tenant;
    use crate::tenant_publish::plan;
    use crate::tenant_publish::stub::*;
    use boss_testing::scratch::scratch_dir;

    /// A REGISTRY THAT DID NOT ANSWER IS NOT AN EMPTY ONE (backlog
    /// 10776b6c). `rows_of` turned any body that was not a list into
    /// zero rows, and the dispatcher answers a failed rule load with a
    /// 200 carrying `"error"` beside `"rules": []` — so an export read
    /// in that moment wrote a tenant with no rules, well-formed and
    /// wrong. Every such body now refuses, naming what came back.
    #[test]
    fn a_body_that_is_no_list_refuses_rather_than_exporting_nothing() {
        for body in [
            json!({"error": "load dispatcher_rules: pool timed out", "rules": []}),
            json!({"error": "no such registry"}),
            json!({"data": {"not": "a list"}}),
            json!("a string"),
        ] {
            let why = rows_of(body.clone()).expect_err("not a list").to_string();
            assert!(
                why.contains("cannot be read as zero") || why.contains("error"),
                "{body}: {why}"
            );
        }
    }

    /// A PAGE IS NOT THE REGISTRY (backlog 6cf47547). `/api/agents` and
    /// `/api/sensors` answer `{data, total}`; if either ever paged by
    /// default, the export read one page and rewrote the repo's seed
    /// file with the rows it happened to get. A body whose `total`
    /// counts more rows than it carries now refuses, and the refusal
    /// names the door — served here by a real socket, because the door
    /// name is added by `list`, not by the rows helper.
    #[test]
    fn a_short_page_with_a_larger_total_refuses_rather_than_exporting_less() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            use std::io::{Read, Write};
            let (mut sock, _) = listener.accept().unwrap();
            let mut buf = [0u8; 4096];
            let _ = sock.read(&mut buf);
            let body = json!({"data": [{"id": "agent-a"}], "total": 2}).to_string();
            let resp = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n\
                 content-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            sock.write_all(resp.as_bytes()).unwrap();
        });
        let why = list(&seed_client().unwrap(), &base, "/api/agents")
            .expect_err("one row of two is a page, not the registry");
        server.join().unwrap();
        let why = format!("{why:#}");
        assert!(why.contains("/api/agents"), "names the door: {why}");
        assert!(why.contains("1 of 2"), "names the shortfall: {why}");
    }

    /// The three honest shapes still read: a bare array, `{data}`, and
    /// the dispatcher's `{rules}` — an empty one included.
    #[test]
    fn the_list_shapes_still_read() {
        let row = json!({"id": 1});
        assert_eq!(rows_of(json!([row])).unwrap(), vec![row.clone()]);
        assert_eq!(
            rows_of(json!({"data": [row], "total": 1})).unwrap(),
            vec![row.clone()]
        );
        assert_eq!(
            rows_of(json!({"rules": [row], "error": null, "authored_registry": null})).unwrap(),
            vec![row]
        );
        assert!(rows_of(json!({"rules": []})).unwrap().is_empty());
    }

    fn snapshot() -> Snapshot {
        Snapshot {
            tenant_id: "acme".into(),
            company_label: Some("Acme, LLC".into()),
            classes: vec![
                json!({"subject_kind": "employee", "code": "founder", "display_name": "Founder",
                       "parent_code": null, "member_attribute": "role", "metadata": {},
                       "sort_order": 1, "retired_at": null}),
                json!({"subject_kind": "account", "code": "customer", "display_name": "Customer",
                       "parent_code": null, "member_attribute": "account_type", "metadata": {"tier": 1},
                       "sort_order": 10, "retired_at": null}),
            ],
            accounts: vec![
                json!({"id": "x", "code": "1010", "name": "Stripe", "kind": "asset",
                       "normal_side": "debit", "is_active": true, "parent": "1000"}),
                json!({"id": "y", "code": "1000", "name": "Bank", "kind": "asset",
                       "normal_side": "debit", "is_active": true, "parent": null}),
                json!({"id": "z", "code": "2300", "name": "Sales tax payable", "kind": "liability",
                       "normal_side": "credit", "is_active": true, "parent": null}),
            ],
            tax_kinds: vec![json!({"kind": "sales", "liability_account": "2300",
                                   "expense_account": null, "derive_basis": "period-sales-tax"})],
            sales_tax_rates: vec![json!({"state": "WA", "jurisdiction": "US-WA", "rate_bps": 650})],
            locations: vec![
                json!({"id": "loc-hq", "name": "HQ", "kind": "office", "timezone": "UTC",
                       "parent_id": null, "latitude": null, "longitude": null, "address": null,
                       "account_id": null, "metadata": {}, "retired_at": null}),
                json!({"id": "loc-annex", "name": "Annex", "kind": "office", "timezone": "UTC",
                       "parent_id": "loc-hq", "latitude": 1.5, "longitude": null, "address": null,
                       "account_id": null, "metadata": {}, "retired_at": null}),
            ],
            calendars: vec![json!({"code": "acme-founder", "name": "founder hours",
                                   "weekend": [5, 6], "closed": ["2026-12-25"]})],
            policy: vec![
                json!({"id": "founder:step:read", "role": "founder", "resource": "step",
                       "action": "read", "scope": "all", "active": true}),
                json!({"id": "founder:job:read", "role": "founder", "resource": "job",
                       "action": "read", "scope": "all", "active": true}),
                json!({"id": "founder:job:create", "role": "founder", "resource": "job",
                       "action": "create", "scope": "all", "active": true}),
                json!({"id": "clerk:job:read", "role": "clerk", "resource": "job",
                       "action": "read", "scope": {"department": "ops"}, "active": true}),
            ],
            people: vec![
                json!({"id": "emp-david", "name": "David", "email": "d@acme.example",
                                "role": "founder", "department": "engineering", "skill_level": null,
                                "hire_date": "2026-09-16", "location": "loc-hq", "manager_id": null,
                                "employment_type": "full-time", "status": "active", "skills": [],
                                "certifications": [], "annual_salary_cents": 0}),
            ],
            agents: vec![
                json!({"id": "agent-claude", "display_name": "Claude", "default_model": "opus-5[1m]",
                                "role": null, "department": null, "hourly_budget_usd_micros": null,
                                "max_concurrent_runs": null, "aliases": ["claude@acme.example"]}),
            ],
            workflows: vec![json!({
                "kind": "handle-a-request", "version": 1, "status": "active",
                "label": "Handle a request", "description": null, "category": "operations",
                "subject_kinds": ["custom"], "owning_team": "acme", "authoring_job_id": null,
                "created_at": "2026-09-18T00:00:00Z", "metadata": {}, "metadata_schema": {},
                "entitlements": {},
                "steps": [
                    {"title": "received", "kind": "trigger", "ready_when": "true",
                     "title_template": "Request received", "sign_offs_required": [], "fields": [],
                     "authority_role": null,
                     "metadata_defaults": {"trigger_kind": "operator", "trigger_name": "request"}},
                    {"title": "handle", "kind": "task", "ready_when": "steps.received.done",
                     "title_template": "Handle it", "sign_offs_required": [],
                     "fields": [{"name": "result", "field_type": "string", "required": true, "filled_by": "executor"}],
                     "authority_role": "owner", "audience": {"role": "owner"},
                     "metadata_defaults": null},
                    {"title": "handled", "kind": "outcome", "ready_when": "steps.handle.done",
                     "terminal": {"outcome": "handled"}, "title_template": "Handled",
                     "sign_offs_required": [], "fields": [], "authority_role": null,
                     "metadata_defaults": {"outcome_kind": "completed"}},
                ],
            })],
            credentials: vec![
                json!({"id": "stripe-restricted-read", "kind": "stripe-restricted-key",
                                     "issuer": "stripe", "principal": "the account",
                                     "scopes": ["charges: read"], "storage_location": "k8s Secret x",
                                     "consumers": [{"kind": "env", "location": "dispatcher env K"}],
                                     "rotation_policy": "on-demand", "rotated_at": null, "notes": "",
                                     "created_at": "2026-09-18T00:00:00Z"}),
            ],
            sensors: vec![json!({"id": "stripe-sponsorships", "source": "stripe",
                                 "credential": "stripe-restricted-read", "every_minutes": 15,
                                 "opens_kind": "handle-a-request", "subject_kind": "custom",
                                 "enabled": true, "tenant_id": "acme",
                                 "published_at": "2026-09-18T00:00:00Z", "last_polled_at": null,
                                 "cursor_at": null})],
            posting_rules: vec![
                json!({"fact_kind": "finance.sponsorship.received", "version": 1,
                "basis": "cash", "source": "tenant:acme",
                "lines": [
                    {"account_code": "1010", "side": "debit", "amount_path": "/metadata/amount_cents"},
                    {"account_code": "4100", "side": "credit", "amount_path": "/metadata/amount_cents"}
                ]}),
            ],
            projections: vec![json!({"event_kind": "step.done.task",
                                     "when": {"/workflow_kind": "handle-a-request", "/spec_slug": "handled"},
                                     "fact_kind": "finance.sponsorship.received", "source_table": "jobs",
                                     "source_id_path": "/job_id", "happened_on_path": "/completed_on",
                                     "created_by_path": null})],
            rules: vec![
                json!({"name": "complete-site-live-on-converge-closed", "version": 2,
                               "on_event": "jobs.job.closed", "when": "kind = \"maintenance-cluster-converge\"",
                               "do": [{"handler": "messages.notify", "args": {}}], "delay": null,
                               "status": "active", "why": null, "authored": false, "source": "tenant:acme"}),
            ],
        }
    }

    /// The whole point of the shape: every file the export writes is OK
    /// under the contract's own loader, and the export of an unchanged
    /// instance into the directory it just wrote is a no-op diff.
    #[test]
    fn an_export_passes_check_and_a_second_export_is_a_no_op() {
        let dir = scratch_dir("boss-cli-tenant-export-noop");
        let snap = snapshot();
        let first = export_into(&snap, &dir).unwrap();
        assert!(
            first.iter().all(|c| matches!(c.outcome, Outcome::Added)),
            "{first:#?}"
        );
        let report = tenant::check(&dir);
        assert!(
            report.rows.iter().all(|r| r.status == tenant::Status::Ok),
            "{}",
            report.render(&dir)
        );
        let second = export_into(&snap, &dir).unwrap();
        assert!(
            second
                .iter()
                .all(|c| matches!(c.outcome, Outcome::Unchanged)),
            "{second:#?}"
        );
        let out = render_changes(&dir, "acme", &second);
        assert!(out.contains("0 added, 0 changed, 15 unchanged"), "{out}");
    }

    /// Each file is the LOADER's inverse: what the contract reads from
    /// the exported file is what the instance held.
    #[test]
    fn the_exported_files_read_back_as_the_instance_rows() {
        let dir = scratch_dir("boss-cli-tenant-export-readback");
        let snap = snapshot();
        export_into(&snap, &dir).unwrap();

        // Workflows: the spec's steps, audience declared once (the
        // derived authority_role is not written), defaults omitted.
        let specs = boss_jobs::seed_loader::load_workflows_with_owning_team(
            dir.join("seeds/workflows.toml"),
            "acme",
        )
        .unwrap();
        assert_eq!(specs.len(), 1);
        let live: boss_jobs::registry::WorkflowSpec =
            serde_json::from_value(snap.workflows[0].clone()).unwrap();
        assert_eq!(specs[0].steps, live.steps);
        assert_eq!(specs[0].label, live.label);
        let text = std::fs::read_to_string(dir.join("seeds/workflows.toml")).unwrap();
        assert!(
            !text.contains("authority_role"),
            "the audience declares it once:\n{text}"
        );

        // The chart: parent-first, normal_balance spelled as the loader
        // reads it.
        let chart =
            boss_ledger::chart::load_chart_toml(&dir.join("seeds/chart_of_accounts.toml")).unwrap();
        let codes: Vec<&str> = chart.iter().map(|a| a.code.as_str()).collect();
        assert_eq!(
            codes,
            ["1000", "2300", "1010"],
            "parents first, then by code"
        );
        assert_eq!(chart[2].parent.as_deref(), Some("1000"));
        assert_eq!(chart[2].normal_balance, "debit");

        // Grants: (role, scope, resource) groups with sorted actions, a
        // department scope in its db spelling.
        let rules = boss_policy_client::seed_loader::load_policy_rules(
            &dir.join("seeds/policy_rules.toml"),
        )
        .unwrap();
        let mut ids: Vec<&str> = rules.iter().map(|r| r.id.as_str()).collect();
        ids.sort();
        assert_eq!(
            ids,
            [
                "clerk:job:read",
                "founder:job:create",
                "founder:job:read",
                "founder:step:read"
            ]
        );
        assert_eq!(
            rules.iter().find(|r| r.role == "clerk").unwrap().scope,
            boss_policy_client::Scope::Department("ops".into())
        );

        // Agents, sensors, credentials, the ledger's rules, the rules:
        // each through its own loader.
        let agents = boss_jobs::agents::load_agents_toml(&dir.join("seeds/agents.toml")).unwrap();
        assert_eq!(agents[0].aliases, ["claude@acme.example"]);
        assert_eq!(agents[0].role, None);
        let sensors =
            boss_jobs::sensors::load_sensors_toml(&dir.join("seeds/sensors.toml")).unwrap();
        assert_eq!(sensors[0].opens, "handle-a-request");
        assert_eq!(sensors[0].every_minutes, 15);
        let creds =
            boss_jobs::credentials::load_credentials_toml(&dir.join("seeds/credentials.toml"))
                .unwrap();
        assert_eq!(creds[0].consumers.len(), 1);
        let posting = boss_ledger::posting_rules::load_posting_rules_toml(
            &dir.join("seeds/posting_rules.toml"),
        )
        .unwrap();
        assert_eq!(posting[0].lines.len(), 2);
        let projections = boss_ledger::posting_rules::load_projection_rules_toml(
            &dir.join("seeds/fact_projection_rules.toml"),
        )
        .unwrap();
        assert_eq!(
            projections[0].when.as_ref().unwrap()["/spec_slug"],
            "handled"
        );
        let raw = boss_dispatcher::rules::registry::parse_raw_file(&dir.join("seeds/rules.toml"))
            .unwrap();
        assert_eq!(raw.rules[0].version, 2);
        assert_eq!(raw.rules[0].do_steps[0].handler, "messages.notify");
        let tax = boss_ledger::tax_registry::load_tax_toml(&dir.join("seeds/tax.toml")).unwrap();
        assert_eq!(
            tax.tax_kind[0].derive_basis.as_deref(),
            Some("period-sales-tax")
        );
        assert_eq!(tax.sales_tax_rate[0].rate_bps, 650);

        // Locations parent-first; classes sorted by (kind, code) with
        // an empty metadata omitted and a non-empty one kept.
        let locs = std::fs::read_to_string(dir.join("seeds/locations.toml")).unwrap();
        assert!(locs.find("loc-hq").unwrap() < locs.find("loc-annex").unwrap());
        let classes: Vec<Value> =
            serde_json::from_str(&std::fs::read_to_string(dir.join("seeds/classes.json")).unwrap())
                .unwrap();
        assert_eq!(classes[0]["code"], "customer");
        assert_eq!(classes[0]["metadata"]["tier"], 1);
        assert!(classes[1].get("metadata").is_none());
        assert!(classes[1].get("parent_code").is_none());
    }

    /// `tenant.toml` is edited on ONE line: the instance holds the
    /// company's label and nothing else the manifest says.
    #[test]
    fn the_manifest_keeps_every_line_but_display_name() {
        let text = "# the manifest\n[meta]\ntenant_id = \"acme\"\ndisplay_name = \"Acme\"\n\
                    operating_days = [\"mon\"]\n\n[modules]\nfinance = true\n";
        let out = render_manifest(Some(text), "acme", Some("Acme Holdings, LLC"));
        assert_eq!(
            out,
            "# the manifest\n[meta]\ntenant_id = \"acme\"\ndisplay_name = \"Acme Holdings, LLC\"\n\
             operating_days = [\"mon\"]\n\n[modules]\nfinance = true\n"
        );
        assert_eq!(
            render_manifest(Some(text), "acme", Some("Acme")),
            text,
            "the same label is no change"
        );
        assert_eq!(
            render_manifest(Some(text), "acme", None),
            text,
            "an unminted company changes nothing"
        );
        assert_eq!(
            render_manifest(None, "acme", Some("Acme")),
            "[meta]\ntenant_id = \"acme\"\ndisplay_name = \"Acme\"\n"
        );
        // A [meta] without the key gains it; a manifest whose [meta]
        // is last gains it at the end.
        let out = render_manifest(
            Some("[meta]\ntenant_id = \"acme\"\n\n[labels]\n"),
            "acme",
            Some("Acme"),
        );
        assert_eq!(
            out,
            "[meta]\ntenant_id = \"acme\"\n\ndisplay_name = \"Acme\"\n[labels]\n"
        );
    }

    /// A rule's `why` is not in the registry: the file's own is kept,
    /// and a rule with none says it was exported without one.
    #[test]
    fn a_rules_why_is_kept_from_the_file_or_named_as_missing() {
        let dir = scratch_dir("boss-cli-tenant-export-why");
        put(&dir, "seeds/rules.toml", &rules_toml(1, "messages.notify"));
        let mut snap = snapshot();
        snap.rules.push(
            json!({"name": "a-new-reactor", "version": 1, "on_event": "jobs.job.closed",
                               "do": [{"handler": "messages.notify", "args": {}}],
                               "status": "active", "source": "tenant:acme"}),
        );
        export_into(&snap, &dir).unwrap();
        let text = std::fs::read_to_string(dir.join("seeds/rules.toml")).unwrap();
        assert!(
            text.contains("the converge packet's close is the site's evidence"),
            "the file's why survives the export:\n{text}"
        );
        assert!(text.contains("without a recorded why"), "{text}");
        let raw = boss_dispatcher::rules::registry::parse_raw_file(&dir.join("seeds/rules.toml"))
            .unwrap();
        assert_eq!(raw.rules.len(), 2, "check accepts both");
    }

    /// No rows and a file that declares none: left alone, named as
    /// skipped. No rows and a file that declares some: rewritten empty.
    #[test]
    fn an_empty_registry_leaves_a_declaring_nothing_file_alone() {
        let dir = scratch_dir("boss-cli-tenant-export-empty");
        put(&dir, "seeds/tax.toml", "# nothing to file yet\n");
        put(
            &dir,
            "seeds/sensors.toml",
            "[[sensor]]\nid = \"gone\"\nsource = \"stripe\"\ncredential = \"c\"\n\
             every_minutes = 5\nopens = \"x\"\nsubject_kind = \"custom\"\n",
        );
        let mut snap = snapshot();
        snap.tax_kinds.clear();
        snap.sales_tax_rates.clear();
        snap.sensors.clear();
        snap.credentials.clear();
        let changes = export_into(&snap, &dir).unwrap();
        let of = |p: &str| changes.iter().find(|c| c.path == p).unwrap().outcome;
        assert_eq!(of("seeds/tax.toml"), Outcome::Skipped);
        assert_eq!(
            std::fs::read_to_string(dir.join("seeds/tax.toml")).unwrap(),
            "# nothing to file yet\n"
        );
        assert_eq!(of("seeds/sensors.toml"), Outcome::Changed);
        assert!(
            !std::fs::read_to_string(dir.join("seeds/sensors.toml"))
                .unwrap()
                .contains("gone"),
            "a row the instance no longer holds is gone from the file"
        );
        assert_eq!(of("seeds/credentials.toml"), Outcome::Skipped);
        assert!(!dir.join("seeds/credentials.toml").exists());
    }

    /// THE ROUND TRIP (backlog e618f3ac): the real tenant's fixture is
    /// published into the recording stub, exported back out of it
    /// through the list reads, and the exported directory (a) passes
    /// check, (b) publishes into the same stub writing NOTHING new,
    /// and (c) exports again byte-identical. The stub's list routes
    /// answer from the same stores the doors wrote.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_published_fixture_exports_back_and_republishes_as_a_no_op() {
        let fixture = real_shape("export-round-trip");
        let st = stub_for(&fixture);
        let base = spawn_stub(st.clone()).await;
        let p = plan(&fixture).unwrap();
        run_publish(p, base.clone()).await.unwrap();
        let writes = st.lock().unwrap().writes();

        let bases = Bases::resolve(Some(&base));
        let snap = tokio::task::spawn_blocking(move || read_instance(&bases, "acme"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(snap.company_label.as_deref(), Some("Acme, LLC"));
        assert_eq!(snap.workflows.len(), 1, "{:?}", snap.workflows);
        assert_eq!(snap.rules.len(), 1, "{:?}", snap.rules);

        let out = scratch_dir("boss-cli-tenant-export-round-trip-out");
        put(
            &out,
            "tenant.toml",
            "[meta]\ntenant_id = \"acme\"\ndisplay_name = \"Acme, LLC\"\n",
        );
        // The fixture's rules file, so the why rides along.
        put(&out, "seeds/rules.toml", &rules_toml(2, "messages.notify"));
        let changes = export_into(&snap, &out).unwrap();
        let report = tenant::check(&out);
        assert!(
            report.rows.iter().all(|r| r.status == tenant::Status::Ok),
            "{}\n{changes:#?}",
            report.render(&out)
        );

        // (b) publish the export back: every door insert-if-absent
        // finds its rows, so the stub's stores do not grow.
        let p2 = plan(&out).unwrap();
        assert!(p2.publishable(), "{}", p2.render_footer());
        let lines = run_publish(p2, base.clone()).await.unwrap();
        assert_eq!(
            st.lock().unwrap().writes(),
            writes,
            "the re-publish wrote nothing new:\n{}",
            lines.join("\n")
        );
        assert!(
            lines.iter().all(|l| !l.contains("kept:")),
            "nothing differs from the instance:\n{}",
            lines.join("\n")
        );

        // (c) the export is a fixed point.
        let bases = Bases::resolve(Some(&base));
        let again = tokio::task::spawn_blocking(move || read_instance(&bases, "acme"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(again, snap);
        let second = export_into(&again, &out).unwrap();
        assert!(
            second
                .iter()
                .all(|c| matches!(c.outcome, Outcome::Unchanged)),
            "{second:#?}"
        );
    }
}
