//! The vocabulary every registry door answers a publish in (design
//! e187198f, David 2026-09-18: THE INSTANCE IS THE TRUTH; the tenant
//! repo is bootstrap + export).
//!
//! WHY ONE MODULE. Measured that day (agent report on fed7a9e2): four
//! doors overwrote a live row on every republish — business calendars
//! wholesale, the company label, an employee's declared fields, an
//! agent's whole row — and `boss tenant publish` runs at EVERY
//! services-container start, so an operator's edit to any of them was
//! reverted at the next boot. The rest were insert-if-absent, and four
//! of those (classes, sensors, policy, workflows) said NOTHING when the
//! repo's row differed from the live one, so a repo edit that never
//! landed was dead text. The decision: every door is insert-if-absent
//! by default, an overwrite happens only under an explicit
//! [`PublishMode::Take`] the operator names per registry, and every
//! door names its kept-but-differing rows so the disagreement is read
//! on the publish line rather than discovered in production.
//!
//! The shapes here are the answer's grammar — what a batch inserted,
//! what it KEPT that differs ([`KeptRow`]), what a take UPDATED field
//! by field ([`UpdatedRow`]) — so the verb renders every registry's
//! line the same way and a reader learns the rule once.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// What a publish may do to a row the registry already holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PublishMode {
    /// The default: a row already there is kept as it is, and the
    /// answer names every declared field it differs on. A converge
    /// never reverts a live edit.
    #[default]
    InsertIfAbsent,
    /// The declaration overwrites the live row on every declared
    /// field, and the answer names each change from → to. Only ever
    /// sent for a registry an operator named in `--take`.
    Take,
}

impl PublishMode {
    pub fn is_take(self) -> bool {
        matches!(self, PublishMode::Take)
    }

    /// The query-string spelling (`?mode=take`).
    pub fn as_str(self) -> &'static str {
        match self {
            PublishMode::InsertIfAbsent => "insert-if-absent",
            PublishMode::Take => "take",
        }
    }
}

/// `?mode=insert-if-absent|take` on a door — absent is the default,
/// so every caller that predates the parameter (an engine's prepare)
/// gets insert-if-absent without changing.
#[derive(Debug, Clone, Copy, Default, Deserialize)]
pub struct ModeQuery {
    #[serde(default)]
    pub mode: PublishMode,
}

/// One field a take changed on a row the registry already held: the
/// column, what it read, what it reads now. `from`/`to` are JSON so a
/// string, a number, a null and a list render the same way everywhere
/// ([`FieldChange::render`]), and so the fact recording the change
/// carries the values, not a rendering of them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldChange {
    pub field: String,
    pub from: Value,
    pub to: Value,
}

/// A value on a publish line: a string bare, everything else as JSON.
fn show(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

impl FieldChange {
    pub fn new(field: &str, from: impl Serialize, to: impl Serialize) -> Self {
        Self {
            field: field.to_string(),
            from: serde_json::to_value(from).unwrap_or(Value::Null),
            to: serde_json::to_value(to).unwrap_or(Value::Null),
        }
    }

    /// `location loc-hq → loc-algedonic-hq`.
    pub fn render(&self) -> String {
        format!("{} {} → {}", self.field, show(&self.from), show(&self.to))
    }
}

/// A row a take UPDATED to the declaration: the id and every field
/// that changed. One shape for every door and for the verb's own
/// employee overlay — one rule, one rendering.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdatedRow {
    pub id: String,
    pub changes: Vec<FieldChange>,
}

impl UpdatedRow {
    /// `emp-david (location loc-hq → loc-algedonic-hq)`.
    pub fn render(&self) -> String {
        format!(
            "{} ({})",
            self.id,
            self.changes
                .iter()
                .map(FieldChange::render)
                .collect::<Vec<_>>()
                .join(", ")
        )
    }
}

/// `updated 2: emp-david (location loc-hq → loc-algedonic-hq); emp-two
/// (department it → ops)` — the publish line's tail for the rows a
/// take changed. Empty when nothing was.
pub fn render_updated(rows: &[UpdatedRow]) -> String {
    if rows.is_empty() {
        return String::new();
    }
    format!(
        "updated {}: {}",
        rows.len(),
        rows.iter()
            .map(UpdatedRow::render)
            .collect::<Vec<_>>()
            .join("; ")
    )
}

/// A declared row the registry already held and KEPT as it is, with
/// the declared fields it differs on named (never empty on the wire:
/// a row that compares equal is a count, not a finding).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeptRow {
    pub id: String,
    pub differs: Vec<String>,
}

impl KeptRow {
    /// `emp-david differs on location, role`.
    pub fn render(&self) -> String {
        format!("{} differs on {}", self.id, self.differs.join(", "))
    }
}

/// The one shape every registry's publish line names a kept-but-
/// differing row in — the decision surface of design e187198f:
/// `kept: emp-david differs on location (the instance is the truth;
/// --take employees overwrites)`. `take` is the `--take` spelling of
/// the registry, or `None` for a registry no door can overwrite, in
/// which case the tail says so rather than naming a flag that does
/// not exist. Rows with nothing differing are dropped. Empty when
/// nothing is kept-but-differing.
pub fn render_kept(kept: &[KeptRow], take: Option<&str>) -> String {
    let differing: Vec<String> = kept
        .iter()
        .filter(|k| !k.differs.is_empty())
        .map(KeptRow::render)
        .collect();
    if differing.is_empty() {
        return String::new();
    }
    let tail = match take {
        Some(registry) => format!("the instance is the truth; --take {registry} overwrites"),
        None => "the instance is the truth; no door overwrites this registry".to_string(),
    };
    format!("kept: {} ({tail})", differing.join("; "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_mode_defaults_to_insert_if_absent_and_parses_the_query_spelling() {
        let q: ModeQuery = serde_urlencoded_like("").unwrap();
        assert_eq!(q.mode, PublishMode::InsertIfAbsent);
        let q: ModeQuery = serde_urlencoded_like("mode=take").unwrap();
        assert_eq!(q.mode, PublishMode::Take);
        let q: ModeQuery = serde_urlencoded_like("mode=insert-if-absent").unwrap();
        assert_eq!(q.mode, PublishMode::InsertIfAbsent);
        assert!(serde_urlencoded_like("mode=overwrite").is_err());
        assert_eq!(PublishMode::Take.as_str(), "take");
        assert!(PublishMode::Take.is_take() && !PublishMode::default().is_take());
    }

    /// The query string as axum's extractor sees it, without the
    /// extractor: a `k=v` list into the same serde shape.
    fn serde_urlencoded_like(q: &str) -> Result<ModeQuery, serde_json::Error> {
        let mut map = serde_json::Map::new();
        for pair in q.split('&').filter(|p| !p.is_empty()) {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            map.insert(k.to_string(), Value::String(v.to_string()));
        }
        serde_json::from_value(Value::Object(map))
    }

    #[test]
    fn a_kept_line_names_the_row_the_fields_and_the_take_flag() {
        let kept = vec![
            KeptRow {
                id: "emp-david".into(),
                differs: vec!["location".into(), "role".into()],
            },
            KeptRow {
                id: "emp-two".into(),
                differs: vec![],
            },
        ];
        assert_eq!(
            render_kept(&kept, Some("employees")),
            "kept: emp-david differs on location, role (the instance is the truth; --take employees overwrites)"
        );
        assert_eq!(
            render_kept(&kept, None),
            "kept: emp-david differs on location, role (the instance is the truth; no door overwrites this registry)"
        );
        assert_eq!(render_kept(&kept[1..], Some("employees")), "");
        assert_eq!(render_kept(&[], Some("employees")), "");
    }

    #[test]
    fn an_updated_line_renders_each_change_from_to() {
        let rows = vec![UpdatedRow {
            id: "agent-claude".into(),
            changes: vec![
                FieldChange::new("display_name", "Old", "New"),
                FieldChange::new("max_concurrent_runs", Option::<i32>::None, Some(2)),
            ],
        }];
        assert_eq!(
            render_updated(&rows),
            "updated 1: agent-claude (display_name Old → New, max_concurrent_runs null → 2)"
        );
        assert_eq!(render_updated(&[]), "");
    }
}
