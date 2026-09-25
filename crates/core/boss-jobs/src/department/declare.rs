//! A DEPARTMENT AS A TENANT DECLARES IT — the shape the write door
//! takes, the one check every door runs, the one loader `boss tenant
//! check` and `boss tenant publish` share, and the pure rule deciding
//! what a declaration does to a row the registry already holds.
//!
//! WHY (backlog 7edf0e97, car 0 of design 8c3e9599's plan, approved by
//! David 2026-09-25). Until this module the `departments` registry had
//! one method, `list`, and two GET routes: the 13 rows migration
//! `20260919181324-a-department-is-a-subject.sql` seeded into EVERY
//! instance could not be changed. Measured 2026-09-25 on the live
//! instance: the same 13 rows, the demo tenant's shape (production,
//! warehouse, distribution, maintenance …), with no product, design or
//! hosting row — while Algedonic, LLC's approved roster is it, product,
//! design, marketing, sales, support, hosting, finance, executive and
//! people. Registries are the live truth (design e187198f), so a roster
//! is declared through the tenant repo and PUBLISHED — which needs a
//! door to publish through.
//!
//! THE DOOR'S RULES are the platform's (`boss_core::publish`, design
//! e187198f): insert-if-absent by code by default, a held row that
//! differs is KEPT and named, and only `?mode=take` (`boss tenant
//! publish --take departments`) overwrites — every change named from →
//! to. A row is RETIRED by declaring it `retired = true` under take: the
//! row stays (packets and page-audit Subjects carry its code), it leaves
//! `list`, and its code stays taken. The retirement is stamped once;
//! a repeat take keeps the first stamp, because when it was withdrawn
//! is a fact.
//!
//! A CODE IS A URL ROOT SEGMENT. A department's pages are `/<code>`
//! (design 8c3e9599 §5), so a code is refused unless it is a lowercase
//! slug, and refused when it equals a segment the platform already owns
//! at the root — the list in `reserved-root-segments.txt` beside this
//! file, read here through `include_str!` so the door and the pin car 1
//! (163fdf7b) holds against the catalog and the gateway read ONE copy.

use std::path::Path;

use boss_core::publish::{FieldChange, KeptRow, PublishMode, UpdatedRow};
use serde::{Deserialize, Serialize};

/// The reserved segments, as written — one per line, `#` comments.
const RESERVED_FILE: &str = include_str!("reserved-root-segments.txt");

/// The root segments no department code may take, in file order.
pub fn reserved_root_segments() -> Vec<&'static str> {
    RESERVED_FILE
        .lines()
        .map(|l| l.split('#').next().unwrap_or_default().trim())
        .filter(|l| !l.is_empty())
        .collect()
}

/// One department as a tenant declares it (`[[department]]` in
/// `seeds/departments.toml`) and as `POST /api/departments/batch`
/// takes it. `code` and `display_name` are the wire's names for the
/// table's `id` and `label` (the read's names since backlog 80a77466);
/// `deny_unknown_fields`, so a column the registry cannot hold yet —
/// `charter`, `head_employee_id`, a `parent` — is refused by name
/// rather than dropped and believed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DepartmentInput {
    pub code: String,
    pub display_name: String,
    /// A Class code under `(department, function)` — operations,
    /// revenue, support, governance on a fresh instance — checked
    /// against the Class registry at the batch door.
    pub function: String,
    /// Tab order. An omitted value declares 0.
    #[serde(default)]
    pub sort_order: i32,
    /// `true` declares the department withdrawn on this instance: kept
    /// as a row (its code is on packets), absent from the list.
    #[serde(default)]
    pub retired: bool,
}

/// Why a declaration is refused. The same check runs in `boss tenant
/// check`, the batch door and both adapters' callers, so the refusal
/// names the same row everywhere.
pub fn validate_department(d: &DepartmentInput) -> Result<(), String> {
    let slug = d
        .code
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_lowercase())
        && d.code
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if !slug {
        return Err(format!(
            "department {:?}: code must be a lowercase slug — a letter, then letters, digits \
             or hyphens — because it is the department's URL root segment (/<code>, design \
             8c3e9599) and the value a packet carries in metadata.department",
            d.code
        ));
    }
    if reserved_root_segments().contains(&d.code.as_str()) {
        return Err(format!(
            "department {}: `{}` is a reserved root segment — the platform already answers \
             /{} (crates/core/boss-jobs/src/department/reserved-root-segments.txt names every \
             one and why); choose another code",
            d.code, d.code, d.code
        ));
    }
    if d.display_name.trim().is_empty() {
        return Err(format!("department {}: display_name is required", d.code));
    }
    if d.function.trim().is_empty() {
        return Err(format!(
            "department {}: function is required — a Class code under (department, function)",
            d.code
        ));
    }
    Ok(())
}

/// Every row valid, and no code declared twice — the whole batch or
/// nothing, so a registry never holds half a tenant.
pub fn validate_batch(rows: &[DepartmentInput]) -> Result<(), String> {
    for d in rows {
        validate_department(d)?;
    }
    let mut seen = std::collections::BTreeSet::new();
    match rows.iter().find(|d| !seen.insert(d.code.as_str())) {
        Some(twice) => Err(format!("department {} is declared twice", twice.code)),
        None => Ok(()),
    }
}

#[derive(Debug, Deserialize)]
struct DepartmentsFile {
    #[serde(default)]
    department: Vec<DepartmentInput>,
}

/// Parse `seeds/departments.toml`'s text. Every row runs
/// [`validate_batch`] here, so a bad declaration is refused with the
/// row named before it reaches any door.
pub fn parse_departments_toml(text: &str) -> Result<Vec<DepartmentInput>, String> {
    let file: DepartmentsFile = toml::from_str(text).map_err(|e| e.to_string())?;
    validate_batch(&file.department)?;
    Ok(file.department)
}

pub fn load_departments_toml(path: &Path) -> Result<Vec<DepartmentInput>, String> {
    let text =
        std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    parse_departments_toml(&text)
}

/// A row as the registry holds it, in the declaration's terms — what a
/// publish compares and what its facts carry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DepartmentRow {
    pub code: String,
    pub display_name: String,
    pub function: String,
    pub sort_order: i32,
    pub retired: bool,
}

impl From<&DepartmentInput> for DepartmentRow {
    fn from(d: &DepartmentInput) -> Self {
        Self {
            code: d.code.clone(),
            display_name: d.display_name.clone(),
            function: d.function.clone(),
            sort_order: d.sort_order,
            retired: d.retired,
        }
    }
}

impl DepartmentRow {
    /// Each declared field this row reads differently, from → to.
    pub fn changes_to(&self, declared: &DepartmentInput) -> Vec<FieldChange> {
        let mut out = Vec::new();
        if self.display_name != declared.display_name {
            out.push(FieldChange::new(
                "display_name",
                &self.display_name,
                &declared.display_name,
            ));
        }
        if self.function != declared.function {
            out.push(FieldChange::new(
                "function",
                &self.function,
                &declared.function,
            ));
        }
        if self.sort_order != declared.sort_order {
            out.push(FieldChange::new(
                "sort_order",
                self.sort_order,
                declared.sort_order,
            ));
        }
        if self.retired != declared.retired {
            out.push(FieldChange::new("retired", self.retired, declared.retired));
        }
        out
    }
}

/// What one declaration does to the registry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowPlan {
    /// The registry holds no row with this code.
    Insert,
    /// Held and differing, under take: every change, from → to.
    Take(Vec<FieldChange>),
    /// Held and differing, under the default: kept, the fields named.
    Keep(Vec<String>),
    /// Held exactly as declared.
    Unchanged,
}

/// THE RULE, pure, so both adapters decide it identically: the
/// instance is the truth unless the operator named this registry in
/// `--take`.
pub fn plan_row(
    held: Option<&DepartmentRow>,
    declared: &DepartmentInput,
    mode: PublishMode,
) -> RowPlan {
    let Some(held) = held else {
        return RowPlan::Insert;
    };
    let changes = held.changes_to(declared);
    if changes.is_empty() {
        RowPlan::Unchanged
    } else if mode.is_take() {
        RowPlan::Take(changes)
    } else {
        RowPlan::Keep(changes.into_iter().map(|c| c.field).collect())
    }
}

/// What a batch did — the platform's publish grammar (the agents
/// door's shape), so `boss tenant publish` renders it on the same line
/// every registry uses.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DepartmentsBatchOutcome {
    pub received: usize,
    pub inserted: usize,
    pub updated: Vec<UpdatedRow>,
    pub kept: Vec<KeptRow>,
    pub unchanged: usize,
}

impl DepartmentsBatchOutcome {
    /// Fold one row's plan into the outcome.
    pub fn record(&mut self, code: &str, plan: &RowPlan) {
        match plan {
            RowPlan::Insert => self.inserted += 1,
            RowPlan::Take(changes) => self.updated.push(UpdatedRow {
                id: code.to_string(),
                changes: changes.clone(),
            }),
            RowPlan::Keep(differs) => self.kept.push(KeptRow {
                id: code.to_string(),
                differs: differs.clone(),
            }),
            RowPlan::Unchanged => self.unchanged += 1,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn product() -> DepartmentInput {
        DepartmentInput {
            code: "product".into(),
            display_name: "Product".into(),
            function: "operations".into(),
            sort_order: 5,
            retired: false,
        }
    }

    #[test]
    fn a_code_is_a_slug_and_never_a_reserved_root_segment() {
        assert!(validate_department(&product()).is_ok());
        for bad in ["", "Product", "9lives", "-x", "a b", "a/b", "a.html", "é"] {
            let mut d = product();
            d.code = bad.into();
            let why = validate_department(&d).unwrap_err();
            assert!(why.contains("lowercase slug"), "{bad:?}: {why}");
        }
        // Every reserved segment is refused by name — derived from the
        // file, so a line added there is covered here without an edit.
        let reserved = reserved_root_segments();
        assert!(
            reserved.len() >= 20,
            "the file parsed to {} segments: {reserved:?}",
            reserved.len()
        );
        for seg in &reserved {
            let mut d = product();
            d.code = (*seg).into();
            let why = validate_department(&d).unwrap_err();
            assert!(
                why.contains("reserved root segment") && why.contains(&format!("/{seg}")),
                "{seg}: {why}"
            );
        }
        // The segments the design names, each present (design 8c3e9599
        // §5 and §6).
        for named in [
            "api",
            "plugins",
            "simulator",
            "dashboard",
            "break-glass",
            "probe",
            "ics",
            "health",
            "site",
            "inbox",
            "views",
            "schedule",
            "jobs",
            "search",
            "manual",
            "system-model",
        ] {
            assert!(reserved.contains(&named), "{named} is not reserved");
        }
        // A reserved line is itself a valid slug, or no code could ever
        // collide with it and the refusal would be dead text.
        for seg in &reserved {
            assert!(
                seg.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
                "{seg} could never be a code"
            );
        }
        let mut d = product();
        d.function = " ".into();
        assert!(validate_department(&d).unwrap_err().contains("function"));
        let mut d = product();
        d.display_name = String::new();
        assert!(
            validate_department(&d)
                .unwrap_err()
                .contains("display_name")
        );
    }

    const FILE: &str = r#"
[[department]]
code = "it"
display_name = "IT"
function = "operations"
sort_order = 1

[[department]]
code = "warehouse"
display_name = "Warehouse"
function = "operations"
sort_order = 100
retired = true
"#;

    #[test]
    fn the_file_parses_and_a_bad_row_a_twin_or_a_stray_field_is_refused_by_name() {
        let rows = parse_departments_toml(FILE).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].code, "it");
        assert!(!rows[0].retired, "an omitted retired declares a live row");
        assert!(rows[1].retired);
        let twice = format!("{FILE}\n{FILE}");
        assert!(
            parse_departments_toml(&twice)
                .unwrap_err()
                .contains("twice")
        );
        let reserved = FILE.replace("code = \"it\"", "code = \"api\"");
        assert!(
            parse_departments_toml(&reserved)
                .unwrap_err()
                .contains("reserved root segment")
        );
        let stray = format!("{FILE}charter = \"we build it\"\n");
        let why = parse_departments_toml(&stray).unwrap_err();
        assert!(why.contains("charter"), "{why}");
        assert!(parse_departments_toml("").unwrap().is_empty());
    }

    #[test]
    fn the_instance_is_the_truth_unless_the_operator_takes() {
        let held = DepartmentRow::from(&product());
        assert_eq!(
            plan_row(None, &product(), PublishMode::InsertIfAbsent),
            RowPlan::Insert
        );
        assert_eq!(
            plan_row(Some(&held), &product(), PublishMode::Take),
            RowPlan::Unchanged,
            "a take of an identical row changes nothing"
        );
        let mut retire = product();
        retire.retired = true;
        retire.display_name = "Product (retired)".into();
        assert_eq!(
            plan_row(Some(&held), &retire, PublishMode::InsertIfAbsent),
            RowPlan::Keep(vec!["display_name".into(), "retired".into()])
        );
        let RowPlan::Take(changes) = plan_row(Some(&held), &retire, PublishMode::Take) else {
            panic!("a take of a differing row updates it");
        };
        let rendered: Vec<String> = changes.iter().map(FieldChange::render).collect();
        assert_eq!(
            rendered,
            vec![
                "display_name Product → Product (retired)".to_string(),
                "retired false → true".to_string()
            ]
        );
        let mut out = DepartmentsBatchOutcome::default();
        out.record("product", &RowPlan::Take(changes));
        out.record("it", &RowPlan::Keep(vec!["function".into()]));
        out.record("sales", &RowPlan::Insert);
        out.record("qa", &RowPlan::Unchanged);
        assert_eq!(
            (
                out.inserted,
                out.updated.len(),
                out.kept.len(),
                out.unchanged
            ),
            (1, 1, 1, 1)
        );
    }
}
