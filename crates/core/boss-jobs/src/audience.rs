//! A step declares its audience ONCE — design f5ebd2e1 (decided by
//! David 2026-09-11, folded on #385), car 1 (backlog 67a58840).
//!
//! Until this landed a step declared who it was for implicitly, three
//! times over, keyed on three different things: `assignee_id`
//! (`postgres.rs` assignment query, the individual arm), `authority_role`
//! in step metadata (the same query's role arm, and every projected
//! `q.<role>.<kind>` station), and a `StationPredicate` row on `job.kind`
//! + `step.slug` / `step.kind` (`station_queue.rs`). Nothing reconciled
//! them and no surface could tell whether a step had declared an audience
//! at all — so backlog item e67cdd56's `design-review` step was in My
//! Day's endpoint and invisible on `/it/design` at the same time, and on
//! 2026-09-15 six operator decisions had to be assigned by id by hand to
//! reach My Day at all.
//!
//! [`Audience`] is the one declaration: a CLOSED set of shapes, so a
//! step that declares an audience no reader is looking for is impossible
//! rather than unlikely. [`selectors_for`] is the ONE function that
//! derives today's three keys from it; materialisation writes those keys
//! onto the packet ([`crate::registry::materialize_steps_at`]) so every
//! existing reader keeps working unchanged — expand/contract. A step
//! that declares no audience behaves exactly as before.
//!
//! What this car does NOT do (car 2 of the design): `department` as
//! Class-registry data — a department audience parses here and derives
//! NO selector yet, and the publish lint refuses it for that reason
//! (`workflow_lint::check_audience_is_declared_once`) — and the surfaces
//! reading one projection.

use serde::{Deserialize, Serialize};

/// Who a step is for. Externally tagged, so the wire and TOML shape is
/// the design's own: `{ role = "platform-admin" }`,
/// `{ individual = "emp-david" }`, `{ department = "it" }`,
/// `{ station = "design-review" }`. A map with any other key, or with
/// two keys, is not an audience — serde refuses it, and the seed loader
/// names the step (`seed_loader::parse_audience`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Audience {
    /// One named actor — a person or a registered agent, by id.
    Individual(String),
    /// Every holder of a role (Class-registry code): a role queue. A
    /// holder is a person whose `employees.role` is the code OR a
    /// registered agent whose `agents.role` is (backlog ab192a9f) —
    /// the dispatcher's nomination reads both as one roster.
    Role(String),
    /// A department (Class-registry code). Parsed and carried, derives
    /// nothing until car 2 makes department a Class on stations.
    Department(String),
    /// A named station row in the station registry.
    Station(String),
}

impl Audience {
    /// The shape's name as it appears on the wire — for refusals that
    /// need to say which shape was declared without re-serialising.
    pub fn shape(&self) -> &'static str {
        match self {
            Self::Individual(_) => "individual",
            Self::Role(_) => "role",
            Self::Department(_) => "department",
            Self::Station(_) => "station",
        }
    }
}

/// Today's three placement keys, as the projection of one audience.
/// `assignee_id` is the step column the assignment query's individual
/// arm reads; `authority_role` and `station` are step-metadata keys —
/// the first is what the role arm and every `q.<role>.<kind>` station
/// predicate read, the second is new and readable by any station
/// predicate through `step.metadata_equals`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Selectors {
    pub assignee_id: Option<String>,
    pub authority_role: Option<String>,
    pub station: Option<String>,
}

impl Selectors {
    /// No reader derives anything from this audience — the shape is
    /// declared but unroutable (a `department`, until car 2).
    pub fn is_empty(&self) -> bool {
        self.assignee_id.is_none() && self.authority_role.is_none() && self.station.is_none()
    }
}

/// THE derivation: one audience, the three keys every current reader
/// already understands. Pure, total, and the only place the mapping
/// lives — a reader that wants to know who a step is for asks this (or
/// [`crate::registry::StepSpec::selectors`], which falls back to the
/// legacy `authority_role` for a step that declares no audience).
pub fn selectors_for(audience: &Audience) -> Selectors {
    match audience {
        Audience::Individual(id) => Selectors {
            assignee_id: Some(id.clone()),
            ..Default::default()
        },
        Audience::Role(role) => Selectors {
            authority_role: Some(role.clone()),
            ..Default::default()
        },
        Audience::Station(name) => Selectors {
            station: Some(name.clone()),
            ..Default::default()
        },
        // Car 2 (f5ebd2e1 resolution `department-is-data`): a Class on
        // stations names the department that owns them. Nothing reads
        // a department yet, so nothing is derived — and the publish
        // lint refuses the declaration rather than admit a step no
        // queue will ever hold.
        Audience::Department(_) => Selectors::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_audience_is_exactly_one_of_four_shapes() {
        let parse = |s: &str| serde_json::from_str::<Audience>(s);
        assert_eq!(
            parse(r#"{"individual":"emp-david"}"#).unwrap(),
            Audience::Individual("emp-david".into())
        );
        assert_eq!(
            parse(r#"{"role":"platform-admin"}"#).unwrap(),
            Audience::Role("platform-admin".into())
        );
        assert_eq!(
            parse(r#"{"department":"it"}"#).unwrap(),
            Audience::Department("it".into())
        );
        assert_eq!(
            parse(r#"{"station":"design-review"}"#).unwrap(),
            Audience::Station("design-review".into())
        );
        // The set is CLOSED: a shape nobody reads cannot be declared.
        let err = parse(r#"{"team":"it"}"#).unwrap_err().to_string();
        assert!(err.contains("unknown variant `team`"), "{err}");
        // And it is ONE declaration: two keys is two audiences.
        assert!(parse(r#"{"role":"a","individual":"b"}"#).is_err());
        assert!(parse(r#""platform-admin""#).is_err());
    }

    #[test]
    fn the_wire_shape_round_trips_as_the_design_wrote_it() {
        let a = Audience::Role("platform-admin".into());
        let json = serde_json::to_value(&a).unwrap();
        assert_eq!(json, serde_json::json!({"role": "platform-admin"}));
        assert_eq!(serde_json::from_value::<Audience>(json).unwrap(), a);
    }

    #[test]
    fn selectors_derive_from_the_audience_one_key_each() {
        assert_eq!(
            selectors_for(&Audience::Individual("emp-david".into())),
            Selectors {
                assignee_id: Some("emp-david".into()),
                ..Default::default()
            }
        );
        assert_eq!(
            selectors_for(&Audience::Role("platform-admin".into())),
            Selectors {
                authority_role: Some("platform-admin".into()),
                ..Default::default()
            }
        );
        assert_eq!(
            selectors_for(&Audience::Station("design-review".into())),
            Selectors {
                station: Some("design-review".into()),
                ..Default::default()
            }
        );
    }

    #[test]
    fn a_department_derives_nothing_until_car_two() {
        let s = selectors_for(&Audience::Department("it".into()));
        assert!(s.is_empty(), "{s:?}");
        assert!(!selectors_for(&Audience::Role("x".into())).is_empty());
    }
}
