//! Shared primitives for the Subject/Class/Part vocabulary.
//!
//! Decision record: `docs/architecture-decisions.md` §Primitives &
//! information architecture (Subject is a trait, not an enum).
//!
//! # Contents
//!
//! - [`Subject`] — identity-bearing thing. Reports a [`Subject::kind`]
//!   (a short snake_case string like `"asset"`) and a [`Subject::id`]
//!   (the stable identifier). The canonical impl is on
//!   [`crate::job::Subject`] (an enum whose variants carry the
//!   per-kind id directly).
//! - [`Part`] — the role a Subject plays inside another Subject.
//!   Two typed flavors: [`Part::Subject`] (identity-bearing
//!   children) and [`Part::Attribute`] (structured-only).
//! - [`Class`] / [`ClassRef`] — logical grouping of Subjects of a
//!   given kind. Membership predicate + display metadata; not a
//!   Subject itself. Persistence form is the `classes` registry.
//! - [`Location`] / [`LocationRef`] — places, with hierarchy,
//!   timezone, and optional geo. Appears in two grammatical
//!   positions: as a Subject and as an `AttributePart` on others.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::job::{Job, Step};

// ---------------------------------------------------------------------------
// Subject
// ---------------------------------------------------------------------------

/// An identity-bearing thing Boss tracks.
///
/// Every concrete Subject impl reports a stable **kind** (a short
/// snake_case string) and an **id** (the stable identifier the rest
/// of the system uses to reference it). Both are stable for the
/// lifetime of the row; they never change across versions.
///
/// The `kind` + `id` pair is what policy rules, Jobs, events, and
/// the KB all key off.
pub trait Subject {
    /// Short, stable, snake_case identifier for the Subject's kind.
    ///
    /// Closed-enum variants return compile-time literal kind strings.
    /// The Custom variant returns the runtime `custom_kind` string so
    /// per-Subject discriminators survive the write/read round trip.
    /// Returns `&str` (not `&'static str`) so the Custom impl can
    /// borrow from its own field.
    fn kind(&self) -> &str;

    /// The stable identifier for this Subject.
    ///
    /// For `custom` subjects this is the `ref_id` (the kind-specific
    /// opaque string); the `custom_kind` discriminator is reported
    /// through [`kind`] itself.
    fn id(&self) -> &str;
}

// ---------------------------------------------------------------------------
// Part
// ---------------------------------------------------------------------------

/// The role a Subject plays when it's composed inside another Subject.
///
/// Two flavors:
///
/// - [`Part::Subject`] — the child is itself identity-bearing (has
///   kind + id, its own events, projections, Jobs). Any composition
///   where the child has a stable identifier Boss tracks.
/// - [`Part::Attribute`] — the child is structured data only, no
///   independent lifecycle. Carried as a `(key, value)` pair so the
///   KB pattern can render it uniformly without caring about the
///   underlying storage shape.
///
/// An owned serde-tagged enum so readers can return `Vec<Part>`
/// directly over HTTP and callers can hold them past their source's
/// lifetime — the usage pattern KB views on the Svelte side need.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "part_kind", rename_all = "snake_case")]
pub enum Part {
    /// A child Subject — identity-bearing, traversable.
    Subject {
        /// The Subject's kind discriminator (e.g. `"asset"`) —
        /// matches the snake_case strings the `Subject` enum's
        /// serde tag emits, or a runtime custom_kind.
        subject_kind: String,
        /// The Subject's stable id.
        id: String,
    },

    /// A structured-data child — no independent identity.
    Attribute {
        /// Short name for the attribute (e.g. `"line_item"`,
        /// `"certification"`). Used by the KB pattern as a grouping
        /// key when rendering.
        key: String,

        /// The serialised value of this attribute. JSON so any
        /// existing join-table row can be rendered without touching
        /// its schema.
        value: Value,
    },
}

impl Part {
    /// Construct an identity-bearing [`Part::Subject`] from any
    /// [`Subject`] impl. Copies the kind + id out so the Part
    /// outlives the source.
    pub fn subject(s: &dyn Subject) -> Self {
        Self::Subject {
            subject_kind: s.kind().to_string(),
            id: s.id().to_string(),
        }
    }

    /// Construct a structured [`Part::Attribute`] from a key +
    /// JSON-serialisable value.
    pub fn attribute(key: impl Into<String>, value: impl Into<Value>) -> Self {
        Self::Attribute {
            key: key.into(),
            value: value.into(),
        }
    }
}

// ---------------------------------------------------------------------------
// Subject kinds
// ---------------------------------------------------------------------------
//
// Core enumerates no closed set of subject kinds. Tenant-extensible
// kinds register through the `boss-subject-kinds` registry. The wire
// format — `#[serde(tag = "subject_kind", rename_all = "snake_case")]`
// on `job::Subject` — produces snake_case kind strings as a serde
// contract on the type itself, not from a separate const. The
// `Subject` impl lives next to its definition in `job.rs`.

/// Read-side wrapper: a Job plus its Steps, assembled for traversal.
///
/// The storage-layer [`Job`] struct doesn't carry Steps directly —
/// Steps live in a separate table and are fetched through the
/// repository (see `boss-jobs::JobsRepository`). Recursive traversal
/// wants both together, so callers assemble a `JobTree` after the
/// fetch. The wrapper is a zero-cost view: two borrowed references,
/// no heap allocation.
///
/// HTTP responses (`GET /api/jobs/{id}`) already ship Job + Steps as
/// one flattened object; `JobTree` is the Rust-side mirror of that
/// shape for traversal code.
#[derive(Debug, Clone, Copy)]
pub struct JobTree<'a> {
    pub job: &'a Job,
    pub steps: &'a [Step],
}

/// Recursively walk every Step under `root`, descending through
/// `Step::embedded_job` pointers.
///
/// Callers supply a `resolver` closure that turns a [`JobId`] into
/// a [`JobTree`] — typically a `HashMap<JobId, JobTree>` lookup
/// built after a batch repository fetch. The walker invokes `visit`
/// on every Step in the transitive closure, depth-first, pre-order.
///
/// Deliberately non-allocating beyond the `visit` callback — the
/// caller decides what to do with each Step (collect into a Vec,
/// fold a counter, render a UI row).
///
/// Cycle protection is the caller's responsibility. Production code
/// should ensure the embed graph is a DAG; this helper will loop
/// indefinitely if the resolver describes a cycle.
pub fn walk_jobs_recursive<'a>(
    root: JobTree<'a>,
    resolver: &dyn Fn(&crate::job::JobId) -> Option<JobTree<'a>>,
    visit: &mut dyn FnMut(&'a Step),
) {
    // Iterate `steps` directly so the borrow keeps the `'a`
    // lifetime — a boxed `dyn Iterator` would elide it and force
    // `visit` to take a shorter-lived `&Step` than callers want.
    for step in root.steps.iter() {
        visit(step);
        if let Some(embed_id) = &step.embedded_job
            && let Some(child) = resolver(embed_id)
        {
            walk_jobs_recursive(child, resolver, visit);
        }
    }
}

// ---------------------------------------------------------------------------
// Class
// ---------------------------------------------------------------------------

/// A logical grouping of [`Subject`]s, identified by a stable `code`
/// within a `subject_kind`.
///
/// A Class is *not* a Subject — no independent
/// lifecycle, no Jobs, no events
/// — it defines a membership predicate over Subjects of a given kind
/// and carries display + grouping metadata. Tenant-extensible
/// taxonomies live here (employee roles, account types, system
/// categories) instead of as closed Rust enums.
///
/// Mirrors the `classes` table in `infra/postgres/schema/01-registries.sql`.
///
/// **v1 contract.** Membership is attribute-defined: a Subject is in
/// the Class iff its `member_attribute` column equals the Class's
/// `code`. Synthetic Classes (predicate or junction-table membership)
/// are reserved for a later extension; `member_attribute` will be
/// `None` for those.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Class {
    pub subject_kind: String,
    pub code: String,
    pub display_name: String,
    pub parent_code: Option<String>,
    /// For attribute-defined Classes (v1), the column on the Subject
    /// whose value equals `code`. `None` for future synthetic Classes.
    pub member_attribute: Option<String>,
    pub metadata: Value,
    pub sort_order: i32,
    pub retired_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// Compact (`subject_kind`, `code`) reference into the Class registry.
///
/// Use this when you need to point at a Class without carrying the
/// full row (display name, metadata, etc.). Equivalent to a foreign
/// key into `classes`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ClassRef {
    pub subject_kind: String,
    pub code: String,
}

impl ClassRef {
    pub fn new(subject_kind: impl Into<String>, code: impl Into<String>) -> Self {
        Self {
            subject_kind: subject_kind.into(),
            code: code.into(),
        }
    }
}

// ---------------------------------------------------------------------------
// Location
// ---------------------------------------------------------------------------

/// A platform-supplied Subject kind: a place. Has identity,
/// hierarchy, timezone, and optional geo / address.
///
/// See `docs/architecture-decisions.md` §Locations. Mirrors
/// the `locations` table in `infra/postgres/schema/01-registries.sql`. `kind` is
/// validated against the Class registry under
/// `(subject_kind='location', member_attribute='kind')`.
///
/// Location appears in two grammatical positions: as a Subject (this
/// struct, the place itself), and as an `AttributePart` hung on
/// another Subject describing where that Subject is. The "no two
/// places at once" invariant — singleton per
/// `(subject_id, attribute_name)` — is enforced at the write path
/// by the `replace_location_part` helper in
/// `boss-locations-client`, not in this struct.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Location {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub parent_id: Option<String>,
    /// IANA timezone (e.g. `"America/Los_Angeles"`). Required so
    /// downstream consumers can ask "what time is it where this
    /// thing is" without plumbing TZ through every call site.
    pub timezone: String,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    /// Free-text postal address. Not broken into structured columns.
    pub address: Option<String>,
    /// FK target (`accounts(id)`) lives in the future `boss-accounts`
    /// crate — held as a plain `String` until that crate exists.
    pub account_id: Option<String>,
    pub metadata: Value,
    pub retired_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// Compact reference into the Locations registry (`id`-only).
///
/// Use when you need to point at a Location without carrying the
/// full row (timezone, hierarchy, etc.). Equivalent to a foreign
/// key into `locations`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct LocationRef {
    pub id: String,
}

impl LocationRef {
    pub fn new(id: impl Into<String>) -> Self {
        Self { id: id.into() }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::job::Subject as JobSubject;

    fn cases() -> Vec<(JobSubject, &'static str, &'static str)> {
        vec![
            (JobSubject::new("asset", "sys-001"), "asset", "sys-001"),
            (JobSubject::new("account", "acc-042"), "account", "acc-042"),
            (
                JobSubject::new("purchase_order", "po-2026-0101"),
                "purchase_order",
                "po-2026-0101",
            ),
            (
                JobSubject::new("campaign", "cmp-spring-26"),
                "campaign",
                "cmp-spring-26",
            ),
            (
                JobSubject::new("employee", "emp-001"),
                "employee",
                "emp-001",
            ),
            (
                JobSubject::new("vendor", "ven-oem-malt"),
                "vendor",
                "ven-oem-malt",
            ),
            (JobSubject::new("location", "loc-hq"), "location", "loc-hq"),
            (
                // A tenant-defined kind: kind() returns the literal
                // kind string, same wire shape as platform kinds.
                JobSubject::new("batch-review", "br-2026-042"),
                "batch-review",
                "br-2026-042",
            ),
        ]
    }

    #[test]
    fn bridge_reports_expected_kind_and_id_for_each_variant() {
        for (subject, expected_kind, expected_id) in cases() {
            assert_eq!(subject.kind(), expected_kind, "{subject:?}");
            assert_eq!(subject.id(), expected_id, "{subject:?}");
        }
    }

    #[test]
    fn every_kind_matches_serde_tag() {
        // Subject is uniformly (kind, id) — the `.kind()` accessor
        // and the `subject_kind` serde discriminator are always
        // identical, including for tenant-defined kinds.
        for (subject, expected_kind, _) in cases() {
            let json = serde_json::to_value(&subject).unwrap();
            let tag = json
                .get("subject_kind")
                .and_then(|v| v.as_str())
                .expect("subject_kind discriminator present");
            assert_eq!(tag, expected_kind, "{subject:?}");
        }
    }

    #[test]
    fn tenant_defined_kind_round_trips() {
        // tenant-defined kinds are first-class — same wire shape
        // as platform kinds.
        let s = JobSubject::new("batch-review", "br-1");
        assert_eq!(s.kind(), "batch-review");
        let json = serde_json::to_value(&s).unwrap();
        assert_eq!(
            json.get("subject_kind").and_then(|v| v.as_str()),
            Some("batch-review")
        );
        assert_eq!(json.get("id").and_then(|v| v.as_str()), Some("br-1"));
        let parsed: JobSubject = serde_json::from_value(json).unwrap();
        assert_eq!(parsed, s);
    }

    #[test]
    fn trait_object_usable_via_dyn() {
        // Smoke-check that `&dyn Subject` works at the call-site
        // ergonomic level — later waves lean on this heavily.
        let subject = JobSubject::new("asset", "sys-12345");
        let as_dyn: &dyn Subject = &subject;
        assert_eq!(as_dyn.kind(), "asset");
        assert_eq!(as_dyn.id(), "sys-12345");
    }

    #[test]
    fn class_round_trips_through_serde() {
        let c = Class {
            subject_kind: "employee".into(),
            code: "service-tech".into(),
            display_name: "Service Tech".into(),
            parent_code: None,
            member_attribute: Some("role".into()),
            metadata: serde_json::json!({"department": "service", "is_management": false}),
            sort_order: 31,
            retired_at: None,
        };
        let v = serde_json::to_value(&c).unwrap();
        assert_eq!(v["subject_kind"], "employee");
        assert_eq!(v["code"], "service-tech");
        assert_eq!(v["member_attribute"], "role");
        let decoded: Class = serde_json::from_value(v).unwrap();
        assert_eq!(decoded, c);
    }

    #[test]
    fn class_ref_equality_keys_on_both_fields() {
        let a = ClassRef::new("employee", "ceo");
        let b = ClassRef::new("employee", "ceo");
        let c = ClassRef::new("account", "ceo");
        let d = ClassRef::new("employee", "cto");
        assert_eq!(a, b);
        assert_ne!(a, c, "different subject_kind ⇒ different Class");
        assert_ne!(a, d, "different code ⇒ different Class");
    }

    #[test]
    fn location_round_trips_through_serde() {
        let loc = Location {
            id: "loc-001".into(),
            name: "Test Location".into(),
            kind: "office".into(),
            parent_id: Some("loc-region-001".into()),
            timezone: "America/Los_Angeles".into(),
            latitude: Some(37.7599),
            longitude: Some(-122.4148),
            address: Some("123 Test St, San Francisco, CA".into()),
            account_id: Some("acc-001".into()),
            metadata: serde_json::json!({"seats": 24}),
            retired_at: None,
        };
        let v = serde_json::to_value(&loc).unwrap();
        assert_eq!(v["id"], "loc-001");
        assert_eq!(v["kind"], "office");
        assert_eq!(v["timezone"], "America/Los_Angeles");
        let decoded: Location = serde_json::from_value(v).unwrap();
        assert_eq!(decoded, loc);
    }

    #[test]
    fn location_ref_equality_keys_on_id() {
        let a = LocationRef::new("loc-hq");
        let b = LocationRef::new("loc-hq");
        let c = LocationRef::new("loc-field-default");
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn part_carries_subject_or_attribute() {
        // Subject variant — build from any dyn Subject via the helper.
        let source = JobSubject::new("asset", "sys-001");
        let sp = Part::subject(&source);
        match &sp {
            Part::Subject { subject_kind, id } => {
                assert_eq!(subject_kind, "asset");
                assert_eq!(id, "sys-001");
            }
            Part::Attribute { .. } => panic!("wrong variant"),
        }

        // Attribute variant — key + serde_json value.
        let ap = Part::attribute("line_item", serde_json::json!({ "qty": 2, "sku": "HT-01" }));
        match &ap {
            Part::Attribute { key, value } => {
                assert_eq!(key, "line_item");
                assert_eq!(value.get("qty").and_then(|v| v.as_i64()), Some(2));
            }
            Part::Subject { .. } => panic!("wrong variant"),
        }

        // Owned — Parts survive serde round-trip.
        let json = serde_json::to_value(&sp).unwrap();
        assert_eq!(json["part_kind"], "subject");
        let decoded: Part = serde_json::from_value(json).unwrap();
        assert_eq!(decoded, sp);
    }

    // -------------------------------------------------------------
    // JobTree step list + recursive walker
    // -------------------------------------------------------------

    use crate::job::{Job, JobId, Priority, Step, StepId, StepStatus};
    use chrono::NaiveDate;
    use std::collections::HashMap;

    fn mk_job(title: &str) -> Job {
        Job {
            id: JobId::new(),
            kind: "test".into(),
            workflow_version: 1,
            subject: JobSubject::new("asset", "sys-001"),
            title: title.into(),
            owner_id: "emp-001".into(),
            status: crate::job::JobStatus::Open,
            priority: Priority::Standard,
            opened_on: NaiveDate::from_ymd_opt(2026, 4, 23).unwrap(),
            due_on: None,
            closed_on: None,
            metadata: serde_json::Value::Object(Default::default()),
            tags: Vec::new(),
            partition: crate::partition::Partition::Real,
        }
    }

    fn mk_step(job_id: JobId, title: &str, embedded: Option<JobId>) -> Step {
        let mut s = Step::new(job_id, "generic", title, 0);
        s.embedded_job = embedded;
        s
    }

    #[test]
    fn jobtree_exposes_steps_in_order() {
        let job = mk_job("parent");
        let steps = vec![
            mk_step(job.id, "step-a", None),
            mk_step(job.id, "step-b", None),
            mk_step(job.id, "step-c", None),
        ];
        let tree = JobTree {
            job: &job,
            steps: &steps,
        };
        let titles: Vec<&str> = tree.steps.iter().map(|s| s.title.as_str()).collect();
        assert_eq!(titles, vec!["step-a", "step-b", "step-c"]);
    }

    #[test]
    fn walk_jobs_recursive_descends_through_embedded_jobs() {
        // Tree:
        //   parent
        //     ├─ step-a (no embed)
        //     ├─ step-b (embeds child-1)
        //     │    └─ child-1
        //     │         ├─ step-x (embeds grandchild)
        //     │         │    └─ grandchild
        //     │         │         └─ step-deep
        //     │         └─ step-y
        //     └─ step-c (embeds child-2)
        //          └─ child-2
        //               └─ step-z
        let grandchild = mk_job("grandchild");
        let grand_steps = vec![mk_step(grandchild.id, "step-deep", None)];
        let child_1 = mk_job("child-1");
        let child_1_steps = vec![
            mk_step(child_1.id, "step-x", Some(grandchild.id)),
            mk_step(child_1.id, "step-y", None),
        ];
        let child_2 = mk_job("child-2");
        let child_2_steps = vec![mk_step(child_2.id, "step-z", None)];
        let parent = mk_job("parent");
        let parent_steps = vec![
            mk_step(parent.id, "step-a", None),
            mk_step(parent.id, "step-b", Some(child_1.id)),
            mk_step(parent.id, "step-c", Some(child_2.id)),
        ];

        let parent_tree = JobTree {
            job: &parent,
            steps: &parent_steps,
        };
        let mut by_id: HashMap<JobId, JobTree<'_>> = HashMap::new();
        by_id.insert(
            child_1.id,
            JobTree {
                job: &child_1,
                steps: &child_1_steps,
            },
        );
        by_id.insert(
            child_2.id,
            JobTree {
                job: &child_2,
                steps: &child_2_steps,
            },
        );
        by_id.insert(
            grandchild.id,
            JobTree {
                job: &grandchild,
                steps: &grand_steps,
            },
        );

        let mut seen: Vec<String> = Vec::new();
        walk_jobs_recursive(
            parent_tree,
            &|id: &JobId| by_id.get(id).copied(),
            &mut |step: &Step| seen.push(step.title.clone()),
        );

        assert_eq!(
            seen,
            vec![
                "step-a",
                "step-b",
                "step-x",
                "step-deep",
                "step-y",
                "step-c",
                "step-z",
            ],
            "recursive walk missed an embed point or visited in the wrong order"
        );
    }

    #[test]
    fn walk_jobs_recursive_skips_unresolved_embed_ids() {
        // A Step with embedded_job set to a JobId the resolver
        // doesn't know about — the walker should visit the Step
        // but not descend (no panic, no infinite loop).
        let dangling = JobId::new();
        let parent = mk_job("parent");
        let parent_steps = vec![
            mk_step(parent.id, "step-a", Some(dangling)),
            mk_step(parent.id, "step-b", None),
        ];
        let tree = JobTree {
            job: &parent,
            steps: &parent_steps,
        };
        let empty: HashMap<JobId, JobTree<'_>> = HashMap::new();
        let mut seen: Vec<String> = Vec::new();
        walk_jobs_recursive(
            tree,
            &|id: &JobId| empty.get(id).copied(),
            &mut |step: &Step| seen.push(step.title.clone()),
        );
        assert_eq!(seen, vec!["step-a", "step-b"]);
    }

    #[test]
    fn step_embedded_job_defaults_to_none_on_deserialize() {
        // Steps written before the embedded_job field existed need to
        // round-trip cleanly. `#[serde(default)]` must recover them as
        // `embedded_job = None`, not a deserialize error.
        let json = serde_json::json!({
            "id": StepId::new().to_string(),
            "job_id": JobId::new().to_string(),
            "kind": "generic",
            "title": "no embedded job",
            "status": "pending",
            "sort_order": 0,
        });
        let step: Step = serde_json::from_value(json).expect("JSON round-trips");
        assert_eq!(step.embedded_job, None);
        assert_eq!(step.status, StepStatus::Pending);
    }

    // -------------------------------------------------------------
    // Proptest on the recursive-walker laws
    //
    // Formal methods track step 2 — proves walk_jobs_recursive
    // preserves three structural invariants across random trees:
    //
    //   1. Visit-once — every reachable Step is emitted exactly
    //      once; unreachable Steps (behind an unresolved embed)
    //      are not emitted.
    //   2. Pre-order — within a Job, Steps emit in their input
    //      order; embedded-Job children interleave between the
    //      embedding Step and its next sibling.
    //   3. Unresolved-skip — a Step pointing at a JobId the
    //      resolver doesn't know about emits the Step but does
    //      not recurse (no panic, no infinite loop).
    //
    // Generators are strictly bounded (≤ 4 Jobs, ≤ 3 Steps per
    // Job, depth ≤ 2) so the test fleet stays in O(tens) even at
    // the default 256 proptest cases. No heap-free-vec tricks, no
    // recursive strategies — a flat "parent Job + pool of child
    // Jobs" shape keeps the generator deterministic and CBMC-like
    // blowups impossible.
    // -------------------------------------------------------------

    use proptest::collection::vec as pvec;
    use proptest::option;
    use proptest::prelude::*;

    /// One generated Job with its Steps + optional embed targets
    /// referenced by step index. Keeping it split means the
    /// embed-resolution phase can build a real Job → JobTree map
    /// without the generator knowing JobIds ahead of time.
    #[derive(Debug, Clone)]
    struct JobInput {
        title: String,
        step_titles: Vec<String>,
        /// For each step index: Some(idx_into_children) → embed
        /// that child Job; None → no embed. Out-of-range indices
        /// become dangling (tested separately).
        embed_targets: Vec<Option<usize>>,
    }

    fn arb_job_input(max_steps: usize) -> impl Strategy<Value = JobInput> {
        ("[a-z]{1,6}", pvec("[a-z]{1,6}", 0..=max_steps))
            .prop_flat_map(|(title, step_titles)| {
                let n = step_titles.len();
                (
                    Just(title),
                    Just(step_titles),
                    pvec(option::of(0usize..=3), n..=n),
                )
            })
            .prop_map(|(title, step_titles, embed_targets)| JobInput {
                title,
                step_titles,
                embed_targets,
            })
    }

    /// Parent Job + up to 3 child Jobs. The parent's steps may
    /// embed any child by index; children have no embeds (depth
    /// capped at 1 to keep the state space tiny).
    fn arb_tree_input() -> impl Strategy<Value = (JobInput, Vec<JobInput>)> {
        (
            arb_job_input(3),
            pvec(
                arb_job_input(3).prop_map(|mut j| {
                    // Depth-cap: children never embed anything.
                    j.embed_targets = vec![None; j.step_titles.len()];
                    j
                }),
                0..=3,
            ),
        )
    }

    fn materialize<'a>(
        parent_input: &JobInput,
        child_inputs: &[JobInput],
        parent_out: &'a mut Option<(Job, Vec<Step>)>,
        children_out: &'a mut Vec<(Job, Vec<Step>)>,
    ) {
        // First materialise child Jobs so we know their ids before
        // wiring the parent's embeds.
        children_out.clear();
        for ci in child_inputs {
            let job = Job {
                id: JobId::new(),
                kind: "test".into(),
                workflow_version: 1,
                subject: JobSubject::new("asset", "sys-p"),
                title: ci.title.clone(),
                owner_id: "emp".into(),
                status: crate::job::JobStatus::Open,
                priority: Priority::Standard,
                opened_on: NaiveDate::from_ymd_opt(2026, 4, 24).unwrap(),
                due_on: None,
                closed_on: None,
                metadata: serde_json::Value::Object(Default::default()),
                tags: vec![],
                partition: crate::partition::Partition::Real,
            };
            let steps: Vec<Step> = ci
                .step_titles
                .iter()
                .map(|t| mk_step(job.id, t, None))
                .collect();
            children_out.push((job, steps));
        }

        let parent_job = Job {
            id: JobId::new(),
            kind: "test".into(),
            workflow_version: 1,
            subject: JobSubject::new("asset", "sys-p"),
            title: parent_input.title.clone(),
            owner_id: "emp".into(),
            status: crate::job::JobStatus::Open,
            priority: Priority::Standard,
            opened_on: NaiveDate::from_ymd_opt(2026, 4, 24).unwrap(),
            due_on: None,
            closed_on: None,
            metadata: serde_json::Value::Object(Default::default()),
            tags: vec![],
            partition: crate::partition::Partition::Real,
        };
        let parent_steps: Vec<Step> = parent_input
            .step_titles
            .iter()
            .enumerate()
            .map(|(i, t)| {
                let embed_ix = parent_input.embed_targets.get(i).copied().flatten();
                let embed_job = embed_ix
                    .and_then(|ix| children_out.get(ix))
                    .map(|(j, _)| j.id);
                mk_step(parent_job.id, t, embed_job)
            })
            .collect();
        *parent_out = Some((parent_job, parent_steps));
    }

    /// Expected pre-order traversal titles, given the tree shape.
    /// Mirrors walk_jobs_recursive's algorithm exactly — if this
    /// function and the walker ever drift, the proptest catches it.
    fn expected_preorder(
        parent_steps: &[Step],
        children: &[(Job, Vec<Step>)],
        resolver_known: &std::collections::HashSet<JobId>,
    ) -> Vec<String> {
        let mut out = Vec::new();
        for step in parent_steps {
            out.push(step.title.clone());
            if let Some(embed) = &step.embedded_job
                && resolver_known.contains(embed)
                && let Some((_, child_steps)) = children.iter().find(|(j, _)| j.id == *embed)
            {
                for cs in child_steps {
                    out.push(cs.title.clone());
                }
            }
        }
        out
    }

    proptest! {
        #![proptest_config(ProptestConfig {
            cases: 100,
            max_shrink_iters: 500,
            ..ProptestConfig::default()
        })]

        // Visit-once + pre-order at the same time: the walker's
        // output must equal the deterministically-computed
        // expected pre-order. This is the strongest invariant —
        // it implies no duplicates, no misses, and correct order.
        #[test]
        fn prop_walker_matches_expected_preorder(
            tree in arb_tree_input(),
        ) {
            let (parent_input, child_inputs) = tree;
            let mut parent_out: Option<(Job, Vec<Step>)> = None;
            let mut children: Vec<(Job, Vec<Step>)> = Vec::new();
            materialize(&parent_input, &child_inputs, &mut parent_out, &mut children);
            let (parent_job, parent_steps) = parent_out.as_ref().unwrap();

            let mut by_id: HashMap<JobId, JobTree<'_>> = HashMap::new();
            for (job, steps) in &children {
                by_id.insert(job.id, JobTree { job, steps });
            }
            let known: std::collections::HashSet<JobId> =
                by_id.keys().copied().collect();

            let tree = JobTree { job: parent_job, steps: parent_steps };
            let mut visited: Vec<String> = Vec::new();
            walk_jobs_recursive(
                tree,
                &|id: &JobId| by_id.get(id).copied(),
                &mut |s: &Step| visited.push(s.title.clone()),
            );

            let expected = expected_preorder(parent_steps, &children, &known);
            prop_assert_eq!(visited, expected);
        }

        // Unresolved-skip: if the resolver drops a known child,
        // the walker visits the embedding Step but omits that
        // subtree. All other embeds still resolve correctly.
        #[test]
        fn prop_walker_skips_unresolved_embeds(
            tree in arb_tree_input(),
            drop_seed in 0u32..=7,
        ) {
            let (parent_input, child_inputs) = tree;
            let mut parent_out: Option<(Job, Vec<Step>)> = None;
            let mut children: Vec<(Job, Vec<Step>)> = Vec::new();
            materialize(&parent_input, &child_inputs, &mut parent_out, &mut children);
            let (parent_job, parent_steps) = parent_out.as_ref().unwrap();

            // Drop every (drop_seed+1)-th child from the resolver.
            // `drop_seed = 0` drops all; `drop_seed = 7` drops about
            // 1/8. Covers the full spectrum of missing-resolvers.
            let mut by_id: HashMap<JobId, JobTree<'_>> = HashMap::new();
            let drop_every = drop_seed as usize + 1;
            for (i, (job, steps)) in children.iter().enumerate() {
                if i % drop_every == 0 {
                    continue; // dropped
                }
                by_id.insert(job.id, JobTree { job, steps });
            }
            let known: std::collections::HashSet<JobId> =
                by_id.keys().copied().collect();

            let tree = JobTree { job: parent_job, steps: parent_steps };
            let mut visited: Vec<String> = Vec::new();
            walk_jobs_recursive(
                tree,
                &|id: &JobId| by_id.get(id).copied(),
                &mut |s: &Step| visited.push(s.title.clone()),
            );

            let expected = expected_preorder(parent_steps, &children, &known);
            prop_assert_eq!(visited, expected);
        }
    }
}
