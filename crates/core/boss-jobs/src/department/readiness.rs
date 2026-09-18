//! WHICH OF THE SIX A DEPARTMENT HAS — the pure half of
//! `GET /api/departments/<code>/readiness` (design 3613f0af, backlog
//! 1dffde5d).
//!
//! IT was filled out in one order and it worked: in / working / out
//! SURFACES, then a SENSOR (a reading outside the audit log), a RULE
//! that turns a reading into a packet, a PROTOCOL with terminals and
//! required evidence, a PROBE that proves each change in production,
//! and a RETRO that counts what was done by hand. Nothing about that
//! order is IT-specific, and "is support filled out yet?" should be a
//! read, not a memory. This module answers it from LIVE registries and
//! nothing else — David, 2026-09-18: seeds are for OSS bootstrap and
//! the playground; the instance runs on data.
//!
//! Every function here is a function of registry rows handed in; the
//! HTTP half (`super::http`) does the reading. Each of the six answers
//! `has: true | false | null` — and `null` ALWAYS carries a `reason`,
//! because a part the read could not judge (no registry wired, a join
//! the record does not hold) must not read as absent. Absent is a
//! finding; undetermined is a different fact, answered as one.

use boss_core::job::{Job, JobStatus};
use boss_core::primitives::Class;
use serde::Serialize;
use serde_json::Value;

use crate::registry::WorkflowSpec;
use crate::sensors::types::SensorRow;

/// The `member_attribute` a department Class carries, and the
/// `subject_kind` it is a Class of: an employee's `department` column.
pub const SUBJECT_KIND: &str = "employee";
pub const MEMBER_ATTRIBUTE: &str = "department";

/// One department as the classes registry declares it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Department {
    pub code: String,
    pub display_name: String,
}

/// The departments the registry holds: the active `employee` Classes
/// whose `member_attribute` is `department`, in the registry's own
/// `sort_order` then by code. Retired rows are not departments.
pub fn departments(classes: &[Class]) -> Vec<Department> {
    let mut rows: Vec<&Class> = classes
        .iter()
        .filter(|c| c.subject_kind == SUBJECT_KIND)
        .filter(|c| c.member_attribute.as_deref() == Some(MEMBER_ATTRIBUTE))
        .filter(|c| c.retired_at.is_none())
        .collect();
    rows.sort_by(|a, b| (a.sort_order, &a.code).cmp(&(b.sort_order, &b.code)));
    rows.into_iter()
        .map(|c| Department {
            code: c.code.clone(),
            display_name: c.display_name.clone(),
        })
        .collect()
}

/// A sensor whose readings open one of the department's kinds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SensorOpening {
    pub id: String,
    pub source: String,
    pub opens: String,
    pub enabled: bool,
}

/// The sensors that open a kind in `kinds`. Disabled sensors are
/// listed with `enabled: false` — a declared sensor that is switched
/// off is a different reading from no sensor at all.
pub fn sensors_opening(sensors: &[SensorRow], kinds: &[String]) -> Vec<SensorOpening> {
    sensors
        .iter()
        .filter(|s| kinds.contains(&s.opens_kind))
        .map(|s| SensorOpening {
            id: s.id.clone(),
            source: s.source.clone(),
            opens: s.opens_kind.clone(),
            enabled: s.enabled,
        })
        .collect()
}

/// A dispatcher rule that opens one of the department's kinds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RuleOpening {
    pub name: String,
    pub version: i64,
    /// Who declared it: `product`, or `tenant:<id>` — the label
    /// `GET /api/dispatcher/rules` serves per rule.
    pub source: String,
    pub opens: Vec<String>,
}

/// The rules, as `GET /api/dispatcher/rules` serves them, whose `do`
/// steps spawn a packet of a kind in `kinds`.
///
/// A rule opens a kind when a `jobs.spawn` step names it as a string
/// LITERAL — the same reading `cadence_roster::string_literal` makes
/// in the dispatcher's silence sweep. A computed kind (an expression
/// over the payload) has no fixed value this read may pretend to know,
/// so it is not counted; a sensor's poll opens the kind its ROW
/// declares, which is why sensors are read from their own registry
/// and not from here.
pub fn rules_opening(rules: &[Value], kinds: &[String]) -> Vec<RuleOpening> {
    rules
        .iter()
        .filter_map(|r| {
            let name = r.get("name")?.as_str()?.to_string();
            let mut opens: Vec<String> = r
                .get("do")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter(|d| d.get("handler").and_then(Value::as_str) == Some("jobs.spawn"))
                .filter_map(|d| {
                    d.get("args")?
                        .get("kind")?
                        .as_str()
                        .and_then(string_literal)
                })
                .filter(|k| kinds.contains(k))
                .collect();
            opens.sort();
            opens.dedup();
            if opens.is_empty() {
                return None;
            }
            Some(RuleOpening {
                name,
                version: r.get("version").and_then(Value::as_i64).unwrap_or(0),
                source: r
                    .get("source")
                    .and_then(Value::as_str)
                    .unwrap_or("product")
                    .to_string(),
                opens,
            })
        })
        .collect()
}

/// A rule arg that is a plain double-quoted string literal, unwrapped;
/// `None` for anything computed per firing.
fn string_literal(src: &str) -> Option<String> {
    let inner = src.trim().strip_prefix('"')?.strip_suffix('"')?;
    if inner.contains('"') {
        return None;
    }
    Some(inner.to_string())
}

/// The newest terminal of one kind: the most recently closed packet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NewestTerminal {
    pub id: String,
    pub title: String,
    /// `metadata.outcome` as the terminal step stamped it, or `null`
    /// for a packet closed without one.
    pub outcome: Option<String>,
    pub closed_on: Option<chrono::NaiveDate>,
}

impl NewestTerminal {
    pub fn of(job: &Job) -> Self {
        Self {
            id: job.id.to_string(),
            title: job.title.clone(),
            outcome: job
                .metadata
                .get("outcome")
                .and_then(Value::as_str)
                .map(str::to_string),
            closed_on: job.closed_on,
        }
    }
}

/// One of the department's protocols, with what its packets say.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct KindReadiness {
    pub kind: String,
    pub version: i32,
    pub label: String,
    /// Every packet of the kind, any status.
    pub packets: i64,
    /// Packets still open.
    pub open: i64,
    pub newest_terminal: Option<NewestTerminal>,
}

/// The kinds whose active row declares `code` — the same join the
/// department jobs view narrows on (`super::kinds_declaring`), with
/// each row's version and label alongside.
pub fn protocols_of<'a>(specs: &'a [WorkflowSpec], code: &str) -> Vec<&'a WorkflowSpec> {
    let kinds = super::kinds_declaring(specs, code);
    kinds
        .iter()
        .filter_map(|k| specs.iter().find(|s| &s.kind == k))
        .collect()
}

/// The newest retro of this department, as the read reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NewestRetro {
    pub id: String,
    pub status: JobStatus,
    pub opened_on: chrono::NaiveDate,
    /// The precise admission stamp when the packet carries one.
    pub opened_at: Option<String>,
    pub closed_on: Option<chrono::NaiveDate>,
}

impl NewestRetro {
    pub fn of(job: &Job) -> Self {
        Self {
            id: job.id.to_string(),
            status: job.status,
            opened_on: job.opened_on,
            opened_at: job
                .metadata
                .get("opened_at")
                .and_then(Value::as_str)
                .map(str::to_string),
            closed_on: job.closed_on,
        }
    }
}

/// One of the six parts, judged. `has: null` means UNDETERMINED and
/// `reason` says why; it is never "no".
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Part {
    pub has: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(flatten)]
    pub detail: Value,
}

impl Part {
    pub fn judged(has: bool, detail: Value) -> Self {
        Self {
            has: Some(has),
            reason: None,
            detail,
        }
    }

    pub fn undetermined(reason: impl Into<String>, detail: Value) -> Self {
        Self {
            has: None,
            reason: Some(reason.into()),
            detail,
        }
    }
}

/// The whole answer.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Readiness {
    pub department: Department,
    pub surfaces: Part,
    pub sensors: Part,
    pub rules: Part,
    pub protocols: Part,
    pub probes: Part,
    pub retro: Part,
    /// How many of the six read `true`, how many could be judged at
    /// all, and which could not — so a reader never adds the six by
    /// hand and never mistakes `null` for `false`.
    pub have: usize,
    pub of: usize,
    pub undetermined: Vec<&'static str>,
}

impl Readiness {
    /// Assemble the six and count them. The count is derived here, once,
    /// from the parts themselves.
    #[allow(clippy::too_many_arguments)]
    pub fn assemble(
        department: Department,
        surfaces: Part,
        sensors: Part,
        rules: Part,
        protocols: Part,
        probes: Part,
        retro: Part,
    ) -> Self {
        let parts = [
            ("surfaces", &surfaces),
            ("sensors", &sensors),
            ("rules", &rules),
            ("protocols", &protocols),
            ("probes", &probes),
            ("retro", &retro),
        ];
        let have = parts.iter().filter(|(_, p)| p.has == Some(true)).count();
        let undetermined: Vec<&'static str> = parts
            .iter()
            .filter(|(_, p)| p.has.is_none())
            .map(|(n, _)| *n)
            .collect();
        let of = parts.len();
        Self {
            department,
            surfaces,
            sensors,
            rules,
            protocols,
            probes,
            retro,
            have,
            of,
            undetermined,
        }
    }
}

/// The surfaces part never needs a registry: the department jobs view
/// (`GET /api/jobs?department=<code>`, landed 2026-09-18) exists for
/// every declared department, so `has` is true by construction and
/// the detail says which view that is. Per design 3613f0af, pages stay
/// data-only until the visual redesign.
pub fn surfaces_part(code: &str) -> Part {
    Part::judged(
        true,
        serde_json::json!({
            "jobs_view": format!("/api/jobs?department={code}"),
            "note": "the department jobs view exists for every declared department; \
                     per design 3613f0af the surface stays data-only until the visual redesign",
        }),
    )
}

/// The probes part is undetermined by construction today, and says so:
/// a proven car (`ship-a-change`) records its branch and its probe, not
/// the workflow kinds it changed, so nothing in the record joins a
/// proof to a department's kinds. A guess here would be the "confident
/// and wrong" answer CLAUDE.md §Doors names.
pub fn probes_part() -> Part {
    Part::undetermined(
        "not derivable from the record: a proven car (ship-a-change) carries its branch and \
         probe, not the workflow kinds it changed, so no join from proofs to a department's \
         kinds exists yet",
        serde_json::json!({}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn class(code: &str, attribute: &str, sort: i32, retired: bool) -> Class {
        Class {
            subject_kind: "employee".into(),
            code: code.into(),
            display_name: code.to_uppercase(),
            parent_code: None,
            member_attribute: Some(attribute.into()),
            metadata: Value::Null,
            sort_order: sort,
            retired_at: retired.then(chrono::Utc::now),
        }
    }

    #[test]
    fn departments_are_the_active_employee_classes_on_the_department_attribute() {
        let rows = vec![
            class("sales", "department", 20, false),
            class("finance", "department", 10, false),
            class("engineer", "role", 5, false),
            class("shuttered", "department", 1, true),
        ];
        let got = departments(&rows);
        assert_eq!(
            got.iter().map(|d| d.code.as_str()).collect::<Vec<_>>(),
            vec!["finance", "sales"],
            "sorted by sort_order; a role is not a department; a retired row is gone"
        );
        assert_eq!(got[0].display_name, "FINANCE");
    }

    fn rule(name: &str, source: Option<&str>, spawns: &[&str]) -> Value {
        let steps: Vec<Value> = spawns
            .iter()
            .map(|k| serde_json::json!({ "handler": "jobs.spawn", "args": { "kind": k } }))
            .collect();
        let mut r = serde_json::json!({ "name": name, "version": 2, "do": steps });
        if let Some(s) = source {
            r["source"] = Value::String(s.into());
        }
        r
    }

    #[test]
    fn a_rule_opens_a_kind_only_by_a_literal_jobs_spawn_arg() {
        let kinds = vec!["receive-an-inquiry".to_string()];
        let rules = vec![
            rule(
                "inquiry-follow-up",
                Some("tenant:algedonic"),
                &["\"receive-an-inquiry\""],
            ),
            // A computed kind: no fixed value to count.
            rule("computed", None, &["payload.kind"]),
            // Another department's kind.
            rule(
                "payout",
                Some("tenant:algedonic"),
                &["\"receive-a-payout\""],
            ),
            // Not a spawn at all.
            serde_json::json!({ "name": "census", "version": 1,
                "do": [{ "handler": "network.census", "args": {} }] }),
        ];
        let got = rules_opening(&rules, &kinds);
        assert_eq!(
            got,
            vec![RuleOpening {
                name: "inquiry-follow-up".into(),
                version: 2,
                source: "tenant:algedonic".into(),
                opens: vec!["receive-an-inquiry".into()],
            }]
        );
        // A product rule reads `product`, the dispatcher's own label.
        let product = rules_opening(&[rule("p", None, &["\"receive-an-inquiry\""])], &kinds);
        assert_eq!(product[0].source, "product");
    }

    #[test]
    fn the_count_is_derived_from_the_parts_and_null_is_not_false() {
        let r = Readiness::assemble(
            Department {
                code: "sales".into(),
                display_name: "Sales".into(),
            },
            surfaces_part("sales"),
            Part::judged(false, serde_json::json!({ "sensors": [] })),
            Part::judged(true, serde_json::json!({ "rules": [] })),
            Part::judged(true, serde_json::json!({ "kinds": [] })),
            probes_part(),
            Part::undetermined("no jobs", serde_json::json!({})),
        );
        assert_eq!((r.have, r.of), (3, 6));
        assert_eq!(r.undetermined, vec!["probes", "retro"]);
        let v = serde_json::to_value(&r).expect("serializes");
        assert_eq!(v["surfaces"]["has"], true);
        assert_eq!(v["sensors"]["has"], false);
        assert_eq!(v["probes"]["has"], Value::Null);
        assert!(
            v["probes"]["reason"]
                .as_str()
                .is_some_and(|s| s.contains("not derivable")),
            "an undetermined part says why"
        );
        assert!(
            v["sensors"].get("reason").is_none(),
            "a judged part carries no reason"
        );
        assert_eq!(
            v["surfaces"]["jobs_view"], "/api/jobs?department=sales",
            "the surfaces detail is flattened beside `has`"
        );
    }
}
