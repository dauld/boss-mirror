//! Q's registry, projected from P's protocols.
//!
//! David, 2026-08-16: *"Q should have a registry of every required
//! station, which are both concrete actor queues and constraint-based
//! queues where any actor that meets constraints can act against it.
//! We have lots of protocols with steps with constraints, so we should
//! have lots of stations to show."*
//!
//! THE MEASUREMENT THAT FORCED THIS. 45 active Workflows carry 302
//! steps, 178 of which declare an `authority_role`, across 22 distinct
//! roles and **51 distinct `(step-kind, role)` constraints**. Three
//! stations had been authored by hand. The gap is not neglect: a step
//! that says "a `bookkeeper` does a `bill-approval`" has ALREADY
//! declared a queue, and writing a station row to say it again is
//! CLAUDE.md §9a's fact living twice. Hand-authoring fifty-one of them
//! was never going to happen, and it didn't.
//!
//! So a constraint station is not authored. It is a projection of the
//! protocol set, and it is regenerated when the protocols change.
//!
//! WHAT THIS IS NOT. Authored stations survive, because some queues
//! are not implied by any step constraint. `my-watchlist` is the
//! worked example — "packets I filed" is not a step constraint and
//! never will be — and `loading-dock` is the train's dock, an
//! operational bundling point rather than a protocol fact. The
//! projection therefore ADDS to the registry rather than replacing it,
//! and a derived station never silently overwrites an authored row of
//! the same name (see [`derived_stations`]).
//!
//! THE SECOND PROJECTION — `(role, model)` (design c87fb59b car 3,
//! backlog cb78818d). Car 1 put an `agent` block on a step, and its
//! projection writes `agent_model` onto the packet beside
//! `authority_role`. A step that says "a `platform-admin` on
//! opus-5[1m] does this" has declared a queue for exactly that
//! capability the way `(kind, role)` declares a constraint queue, and
//! it is projected the same way: one station per distinct pair the
//! active protocols declare, named `a.<role>.<model-slug>`, predicate
//! on both projected keys, capability `{ roles, models }` — the half
//! the dispatcher's executor lane and the claim door read to decide
//! which registered agent serves it (an agents row serves the station
//! when its role and `default_model` match). Nine agent blocks across
//! five platform protocols on 2026-09-18, all `(platform-admin,
//! opus-5[1m])`: one station, which is the point.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};

use boss_core::job::{JobStatus, StepStatus};

use crate::registry::{WorkflowSpec, WorkflowStatus};
use crate::station_queue::{StationPredicate, StepMatch, default_discipline};
use crate::stations::{StationCapability, StationKind, StationSpec};

/// One constraint a protocol declares: this kind of step, waiting for
/// an actor holding this role.
///
/// Deliberately `(kind, role)` and not `(workflow, step)`. A queue is
/// about who can act, not about which protocol asked — a `bookkeeper`
/// clearing `bill-approval` steps does not care whether the packet is
/// an expense bill or a vendor invoice, and modelling it per-workflow
/// would give that person four queues holding one job each. It is also
/// the difference that would have caught a real miss: the authored
/// `design-review` station names `kind = "design-doc-review"`, so the
/// `design-doc` packets introduced the same day matched no station at
/// all.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Constraint {
    pub step_kind: String,
    pub role: String,
}

/// Every constraint the active protocol set declares.
pub fn constraints_of(workflows: &[WorkflowSpec]) -> BTreeSet<Constraint> {
    workflows
        .iter()
        .filter(|w| w.status == WorkflowStatus::Active)
        .flat_map(|w| w.steps.iter())
        // Through `selectors()`, not the raw key: a step that declares
        // `audience = { role = ... }` and nothing else has declared
        // this queue exactly as a legacy `authority_role` does
        // (f5ebd2e1 car 1).
        .filter_map(|s| {
            s.selectors().authority_role.map(|role| Constraint {
                step_kind: s.kind.clone(),
                role,
            })
        })
        .collect()
}

/// The station name a constraint projects to.
///
/// `q.<role>.<step-kind>` — namespaced so a derived row can never
/// collide with an authored one by accident, and readable enough that
/// an operator seeing it in a log knows what it is without a lookup.
///
/// DOTS, NOT SLASHES, and the reason is not taste. The name is a path
/// segment in `GET /api/stations/{name}/queue`; `q/platform-admin/…`
/// would store happily (no charset constraint on `stations.name`) and
/// then match no route at all, so every derived queue would 404 while
/// the listing advertised it. Roles and step kinds are kebab-case, so
/// a dot is also unambiguous — the name can be split back apart.
/// Pinned by `a_derived_name_survives_a_url_path_segment`.
pub fn station_name(c: &Constraint) -> String {
    format!("q.{}.{}", c.role, c.step_kind)
}

/// One `(role, model)` an `agent` block declares under a role: an agent
/// of this role, running this model, does this step. Not `(kind, role,
/// model)`: the model is the capability, and an agent that runs it
/// serves every step kind the role serves.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct AgentConstraint {
    pub role: String,
    pub model: String,
}

/// Every `(role, model)` the active protocol set declares. A block
/// under no role projects nothing: there is no role to gate the claim
/// on, and a station gated on a model alone would hand any agent's
/// work to any other.
pub fn agent_constraints_of(workflows: &[WorkflowSpec]) -> BTreeSet<AgentConstraint> {
    workflows
        .iter()
        .filter(|w| w.status == WorkflowStatus::Active)
        .flat_map(|w| w.steps.iter())
        .filter_map(|s| {
            let agent = s.agent.as_ref()?;
            let role = s.selectors().authority_role?;
            Some(AgentConstraint {
                role,
                model: agent.model.clone(),
            })
        })
        .collect()
}

/// A rate-card model as a URL path segment: `opus-5[1m]` → `opus-5-1m`.
/// The brackets the card spells the context window with are not
/// path-safe (`a_derived_name_survives_a_url_path_segment`), so the
/// name carries a slug and the capability carries the model as
/// spelled — the name is for routes and logs, the capability is what
/// the claim compares.
pub fn model_slug(model: &str) -> String {
    let mut out = String::with_capacity(model.len());
    for c in model.chars().flat_map(char::to_lowercase) {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_matches('-').to_string()
}

/// `a.<role>.<model-slug>` — its own namespace beside `q.`, so a
/// `(role, model)` station can never collide with a `(role, kind)` one
/// even when a model and a step kind share a spelling.
pub fn agent_station_name(c: &AgentConstraint) -> String {
    format!("a.{}.{}", c.role, model_slug(&c.model))
}

/// Project the `(role, model)` pairs into the stations they require —
/// the same merge rule as [`derived_stations`] (authored wins), the
/// same actionable-only status filter, and a predicate on BOTH
/// projected keys so the queue holds that role's work on that model
/// and nothing else.
pub fn agent_stations(
    workflows: &[WorkflowSpec],
    authored: &[String],
    now: DateTime<Utc>,
) -> Vec<StationSpec> {
    agent_constraints_of(workflows)
        .into_iter()
        .map(|c| {
            let name = agent_station_name(&c);
            (c, name)
        })
        .filter(|(_, name)| !authored.iter().any(|a| a == name))
        .map(|(c, name)| StationSpec {
            title: format!("{} — {}", c.role, c.model),
            name,
            version: 1,
            status: WorkflowStatus::Active,
            kind: StationKind::Constraint,
            predicate: StationPredicate {
                status: Some(JobStatus::Open),
                step: Some(StepMatch {
                    kind: None,
                    metadata_equals: BTreeMap::from([
                        ("authority_role".to_string(), c.role.clone()),
                        (crate::agent_spec::MODEL_KEY.to_string(), c.model.clone()),
                    ]),
                    status_in: vec![StepStatus::Ready, StepStatus::Active],
                    ..Default::default()
                }),
                ..Default::default()
            },
            discipline: default_discipline(),
            wip_limit: None,
            terminal_window_days: None,
            capability: Some(StationCapability {
                roles: vec![c.role.clone()],
                models: vec![c.model.clone()],
            }),
            rollup_parent: None,
            upstream: None,
            lens: None,
            created_at: now,
        })
        .collect()
}

/// Project the protocol set into the stations it requires: every
/// `(kind, role)` constraint queue, then every `(role, model)` agent
/// queue ([`agent_stations`]).
///
/// `authored` is the existing registry. A projected station whose name
/// already exists is DROPPED rather than merged: two sources for one
/// row is the drift this whole change exists to remove, and silently
/// preferring one would hide which. Q1's open edge — where a derived
/// station's `wip_limit` and `discipline` come from — is answered by
/// leaving them at their defaults here, so that an operator wanting
/// something else authors a row and that row visibly wins.
pub fn derived_stations(
    workflows: &[WorkflowSpec],
    authored: &[String],
    now: DateTime<Utc>,
) -> Vec<StationSpec> {
    let mut out = constraint_stations(workflows, authored, now);
    out.extend(agent_stations(workflows, authored, now));
    out
}

/// The `(kind, role)` half of [`derived_stations`].
fn constraint_stations(
    workflows: &[WorkflowSpec],
    authored: &[String],
    now: DateTime<Utc>,
) -> Vec<StationSpec> {
    constraints_of(workflows)
        .into_iter()
        .map(|c| {
            let name = station_name(&c);
            (c, name)
        })
        .filter(|(_, name)| !authored.iter().any(|a| a == name))
        .map(|(c, name)| StationSpec {
            title: format!("{} — {}", c.role, c.step_kind),
            name,
            version: 1,
            status: WorkflowStatus::Active,
            kind: StationKind::Constraint,
            predicate: StationPredicate {
                status: Some(JobStatus::Open),
                step: Some(StepMatch {
                    kind: Some(c.step_kind.clone()),
                    // MATCH THE ROLE, not just the kind. Without this
                    // every `q.<role>.task` station showed the SAME
                    // packets: measured on the live registry, ten
                    // role-scoped task queues each reported exactly 48,
                    // because the predicate matched `kind = task` and
                    // the role lived only in `capability`. Capability
                    // gates who may CLAIM; it does not filter what the
                    // queue HOLDS, and a queue that shows a bookkeeper
                    // the head-brewer's work is not a queue.
                    //
                    // `authority_role` is surfaced into step metadata
                    // at materialisation for the sign-off gate
                    // (`merge_metadata`, pinned by
                    // `materialize_surfaces_authority_role_into_metadata`),
                    // so it is readable here without widening
                    // StepMatch.
                    metadata_equals: BTreeMap::from([(
                        "authority_role".to_string(),
                        c.role.clone(),
                    )]),
                    // Ready OR active: a queue shows what is waiting
                    // AND what is being worked, because an operator
                    // reading load needs both. `pending` is excluded —
                    // a step whose predecessors have not finished is
                    // not waiting for an actor, it is waiting for the
                    // protocol.
                    status_in: vec![StepStatus::Ready, StepStatus::Active],
                    ..Default::default()
                }),
                ..Default::default()
            },
            discipline: default_discipline(),
            wip_limit: None,
            terminal_window_days: None,
            // The role IS the constraint, so it gates the claim. This
            // is the half that makes the station mean something to M:
            // "who may act here" is data, not a convention.
            capability: Some(StationCapability {
                roles: vec![c.role.clone()],
                models: Vec::new(),
            }),
            rollup_parent: None,
            upstream: None,
            lens: None,
            created_at: now,
        })
        .collect()
}

#[cfg(test)]
mod tests {

    /// One floor for one derivation, declared once rather than four times
    /// — the per-test copies were the same fact living four places, which
    /// is what §9a is about, and which this car is about in the first
    /// place. Six against a real ten on 2026-09-11 (off 46 protocols):
    /// low enough that retiring a protocol or two is not an edit here,
    /// high enough that a projection which collapsed would red.
    const QUEUE_FLOOR: usize = 6;
    const QUEUES: &str = "the constraint queues the platform protocol bundle declares \
                          (10 on 2026-09-11, off 46 protocols)";
    use super::*;
    use crate::registry::seedable_platform_workflows;

    fn now() -> DateTime<Utc> {
        DateTime::from_timestamp(1_760_000_000, 0).expect("fixed instant")
    }

    /// TWO PROTOCOLS, ONE QUEUE — measured on the real platform set.
    ///
    /// The 51 constraints in this module's header are the RUNNING
    /// registry, which a unit test cannot reach; this reads the set a
    /// deployment seeds (`seedable_platform_workflows()` — the roster,
    /// empty since 2026-09-11, plus every file in the platform bundle).
    /// The gap between the two is not a defect: it is the reason the
    /// projection reads the live registry at runtime and not this
    /// function.
    ///
    /// It pinned an exact two-name list until 2026-09-11, when
    /// `design-doc-review` was the last protocol to leave the Rust
    /// roster and the "code-seeded half" this measured stopped existing.
    /// Re-pinning the list over the whole bundle would have put a
    /// contended tail line in the path of every protocol car (§9a) while
    /// saying nothing the assertions below do not — so what is pinned is
    /// the property the list was carrying.
    ///
    /// The shared case is DERIVED rather than named. The original list
    /// documented one — `review-design`, declared by both
    /// `design-doc-review` and `design-doc`, where the authored
    /// `design-review` station named `kind = "design-doc-review"` and
    /// therefore missed the second — and naming it back would have been
    /// a kind-name match, which `no-step-kind-match` refuses for the
    /// right reason (step-kind names are data). So this finds every
    /// (role, step kind) two or more protocols declare and asserts each
    /// collapses to one queue: the property holds for whichever pair the
    /// bundle happens to contain, and it cannot quietly become vacuous
    /// because it fails when there is no such pair at all.
    #[test]
    fn the_platform_set_declares_exactly_the_constraints_it_declares() {
        let specs = seedable_platform_workflows();
        let found = constraints_of(&specs);
        assert!(!found.is_empty(), "an empty constraint set proves nothing");

        // (role, step kind) → how many distinct protocols declare it.
        let mut declarers: std::collections::BTreeMap<(&str, &str), Vec<&str>> =
            std::collections::BTreeMap::new();
        for w in &specs {
            for s in w.steps.iter().filter(|s| s.authority_role.is_some()) {
                let role = s.authority_role.as_deref().expect("filtered");
                declarers
                    .entry((role, s.kind.as_str()))
                    .or_default()
                    .push(w.kind.as_str());
            }
        }
        let shared: Vec<_> = declarers.iter().filter(|(_, ws)| ws.len() > 1).collect();
        assert!(
            !shared.is_empty(),
            "no (role, step kind) is declared by two protocols, so the one-queue-for-the-pair \
             property is untested rather than held"
        );
        for ((role, step_kind), protocols) in shared {
            let matching: Vec<&Constraint> = found
                .iter()
                .filter(|c| c.role == *role && c.step_kind == *step_kind)
                .collect();
            assert_eq!(
                matching.len(),
                1,
                "{} protocols declare ({role}, {step_kind}) and must share ONE queue, not \
                 {}: {protocols:?}",
                protocols.len(),
                matching.len()
            );
        }

        // Every constraint names both halves — a queue with no role is
        // not a constraint queue, and a role with no step kind cannot
        // be matched against a packet.
        for c in &found {
            assert!(!c.step_kind.is_empty(), "{c:?} has no step kind");
            assert!(!c.role.is_empty(), "{c:?} has no role");
        }
    }

    /// A derived station never overwrites an authored one.
    ///
    /// This is the whole safety property of adding a second source to
    /// a registry: two rows for one name is the drift the change
    /// exists to remove, so the projection yields rather than merging.
    #[test]
    fn an_authored_name_wins_and_the_derived_row_is_dropped() {
        let wf = seedable_platform_workflows();
        let all = derived_stations(&wf, &[], now());
        assert!(!all.is_empty(), "expected some derived stations");

        let taken = all[0].name.clone();
        let with_conflict = derived_stations(&wf, std::slice::from_ref(&taken), now());
        assert!(
            !with_conflict.iter().any(|s| s.name == taken),
            "`{taken}` is authored, so the projection must not also emit it"
        );
        assert_eq!(
            with_conflict.len(),
            all.len() - 1,
            "exactly one row should have been dropped"
        );
    }

    /// The role is carried to the claim gate, not just to the title.
    ///
    /// A station that displays a constraint but does not enforce it is
    /// decoration. `capability` is what M and the claim CAS read.
    #[test]
    fn the_constraint_reaches_the_capability_gate() {
        let derived = derived_stations(&seedable_platform_workflows(), &[], now());
        boss_testing::assert_roster_floor!(derived, QUEUE_FLOOR, "{QUEUES}");
        for s in derived {
            let cap = s.capability.as_ref().unwrap_or_else(|| {
                panic!(
                    "{} has no capability — its constraint is decorative",
                    s.name
                )
            });
            assert_eq!(
                cap.roles.len(),
                1,
                "{}: one role per constraint queue",
                s.name
            );
            assert!(
                s.name.contains(&cap.roles[0]),
                "{} does not name the role it gates on ({})",
                s.name,
                cap.roles[0]
            );
            assert_eq!(s.kind, StationKind::Constraint);
        }
    }

    /// Pending steps are not queued.
    ///
    /// A step whose predecessors have not finished is waiting for the
    /// PROTOCOL, not for an actor. Including it would make every queue
    /// report a depth nobody can act on, which is the fastest way to
    /// make a load number useless to M.
    #[test]
    fn only_actionable_steps_are_queued() {
        let derived = derived_stations(&seedable_platform_workflows(), &[], now());
        boss_testing::assert_roster_floor!(derived, QUEUE_FLOOR, "{QUEUES}");
        for s in derived {
            let step = s
                .predicate
                .step
                .as_ref()
                .expect("a constraint queue matches on a step");
            assert_eq!(
                step.status_in,
                vec![StepStatus::Ready, StepStatus::Active],
                "{}: queue depth must count actionable steps only",
                s.name
            );
        }
    }

    /// TWO PROTOCOLS, ONE QUEUE — the property the grouping choice
    /// rests on, and the one the two-row platform set cannot show.
    ///
    /// A `bookkeeper` clearing `bill-approval` steps does not care
    /// whether the packet is an expense bill or a vendor invoice.
    /// Grouping per-workflow would hand that person two queues holding
    /// one job each, which is how a queue layer becomes noise.
    #[test]
    fn the_same_constraint_in_two_protocols_is_one_queue() {
        let mut a = seedable_platform_workflows()
            .into_iter()
            .find(|w| !w.steps.is_empty())
            .expect("a platform kind with steps");
        a.status = WorkflowStatus::Active;
        a.steps.truncate(1);
        a.steps[0].kind = "bill-approval".into();
        a.steps[0].authority_role = Some("bookkeeper".into());

        let mut b = a.clone();
        a.kind = "expense-bill".into();
        b.kind = "vendor-invoice".into();

        let found = constraints_of(&[a, b]);
        assert_eq!(found.len(), 1, "two protocols, one constraint: {found:#?}");
        let only = found.iter().next().expect("one constraint");
        assert_eq!(station_name(only), "q.bookkeeper.bill-approval");
    }

    /// A role declared as an AUDIENCE projects the same queue a legacy
    /// `authority_role` does (f5ebd2e1 car 1): the constraint is read
    /// through `StepSpec::selectors`, so a step that declares its
    /// audience once — and nothing else — is not an orphan here.
    #[test]
    fn a_role_audience_projects_a_constraint_queue() {
        let mut wf = seedable_platform_workflows()
            .into_iter()
            .find(|w| !w.steps.is_empty())
            .expect("a platform kind with steps");
        wf.status = WorkflowStatus::Active;
        wf.steps.truncate(1);
        wf.steps[0].kind = "answer-question".into();
        wf.steps[0].authority_role = None;
        wf.steps[0].audience = Some(crate::audience::Audience::Role("platform-admin".into()));

        let found = constraints_of(std::slice::from_ref(&wf));
        let only = found.iter().next().expect("one constraint");
        assert_eq!(found.len(), 1, "{found:#?}");
        assert_eq!(station_name(only), "q.platform-admin.answer-question");

        // An individual audience is not a role queue and projects none.
        wf.steps[0].audience = Some(crate::audience::Audience::Individual("emp-david".into()));
        assert!(constraints_of(std::slice::from_ref(&wf)).is_empty());
    }

    /// A retired protocol stops requiring a queue.
    ///
    /// The projection regenerates, so a superseded Workflow version
    /// must not keep a station alive — otherwise the registry only
    /// ever grows and "every required station" degrades into "every
    /// station ever required".
    #[test]
    fn a_retired_protocol_declares_nothing() {
        let mut wf = seedable_platform_workflows()
            .into_iter()
            .find(|w| w.steps.iter().any(|s| s.authority_role.is_some()))
            .expect("a platform kind with a constrained step");
        assert!(!constraints_of(std::slice::from_ref(&wf)).is_empty());

        wf.status = WorkflowStatus::Retired;
        assert!(
            constraints_of(&[wf]).is_empty(),
            "a retired protocol must not hold a queue open"
        );
    }

    /// A derived name is usable as a URL path segment.
    ///
    /// `GET /api/stations/{name}/queue` routes on ONE segment. A name
    /// carrying `/` stores fine — `stations.name` is bare `TEXT` — and
    /// then matches no route, so the listing would advertise queues
    /// that 404. Nothing else in the stack catches that: the schema
    /// allows it, the lint does not look, and the projection's own
    /// unit tests would stay green.
    #[test]
    fn a_derived_name_survives_a_url_path_segment() {
        let derived = derived_stations(&seedable_platform_workflows(), &[], now());
        boss_testing::assert_roster_floor!(derived, QUEUE_FLOOR, "{QUEUES}");
        for s in derived {
            assert!(
                !s.name.contains('/'),
                "`{}` would not match /api/stations/{{name}}/queue",
                s.name
            );
            assert!(
                s.name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "-._".contains(c)),
                "`{}` needs percent-encoding to appear in a URL",
                s.name
            );
        }
    }

    /// A role's queue holds that role's work, and nobody else's.
    ///
    /// THE BUG THIS PINS. The first version matched only on step kind
    /// and put the role in `capability`. Capability gates who may
    /// CLAIM a step; it does not filter what the queue HOLDS. So on
    /// the live registry ten role-scoped `task` queues each reported
    /// exactly 48 packets — the same 48 — and a bookkeeper's queue
    /// showed the head-brewer's work. Found by measuring station depth
    /// across the network, not by reading the code.
    #[test]
    fn a_derived_queue_matches_the_role_not_just_the_kind() {
        let derived = derived_stations(&seedable_platform_workflows(), &[], now());
        boss_testing::assert_roster_floor!(derived, QUEUE_FLOOR, "{QUEUES}");
        for s in derived {
            let step = s
                .predicate
                .step
                .as_ref()
                .expect("a constraint queue matches a step");
            let role = &s.capability.as_ref().expect("gated").roles[0];
            assert_eq!(
                step.metadata_equals.get("authority_role"),
                Some(role),
                "{} gates on `{role}` but its predicate does not filter by it, \
                 so it would hold every step of that kind regardless of role",
                s.name
            );
        }
    }

    /// Two roles sharing a step kind get queues that cannot collide.
    ///
    /// `task` is the worked case: nine platform roles declare a `task`
    /// step, so this is the difference between nine useful queues and
    /// nine copies of one list.
    #[test]
    fn two_roles_on_one_step_kind_get_disjoint_predicates() {
        let mut a = seedable_platform_workflows()
            .into_iter()
            .find(|w| !w.steps.is_empty())
            .expect("a platform kind with steps");
        a.status = WorkflowStatus::Active;
        a.steps.truncate(1);
        a.steps[0].kind = "task".into();
        let mut b = a.clone();
        a.kind = "brew".into();
        a.steps[0].authority_role = Some("head-brewer".into());
        b.kind = "books".into();
        b.steps[0].authority_role = Some("bookkeeper".into());

        let out = derived_stations(&[a, b], &[], now());
        assert_eq!(out.len(), 2, "one queue per (kind, role): {out:#?}");
        let roles: Vec<_> = out
            .iter()
            .map(|s| {
                s.predicate
                    .step
                    .as_ref()
                    .expect("step match")
                    .metadata_equals
                    .get("authority_role")
                    .cloned()
                    .unwrap_or_default()
            })
            .collect();
        assert_eq!(
            roles,
            vec!["bookkeeper".to_string(), "head-brewer".to_string()]
        );
    }

    /// ONE STATION PER (ROLE, MODEL) — design c87fb59b car 3, backlog
    /// cb78818d. A step whose `agent` block names a model, under a
    /// role, has declared a queue for "an agent of that role on that
    /// model" exactly as a `(kind, role)` pair declares a constraint
    /// queue: two protocols declaring the same pair share one station;
    /// a second model is a second station; a block under no role is no
    /// station (there is no role to gate the claim on). The predicate
    /// filters what the queue HOLDS on both projected keys — the
    /// lesson `a_derived_queue_matches_the_role_not_just_the_kind`
    /// paid for — and the capability carries both halves for the claim.
    #[test]
    fn an_agent_block_projects_one_station_per_role_and_model() {
        use crate::agent_spec::{AgentSpec, Effort};
        let block = |model: &str| AgentSpec {
            profile: "builder".into(),
            model: model.into(),
            budget_usd: 5.0,
            effort: Effort::High,
        };
        let mut a = seedable_platform_workflows()
            .into_iter()
            .find(|w| !w.steps.is_empty())
            .expect("a platform kind with steps");
        a.status = WorkflowStatus::Active;
        a.steps.truncate(1);
        a.steps[0].kind = "task".into();
        a.steps[0].authority_role = Some("platform-admin".into());
        a.steps[0].audience = None;
        a.steps[0].agent = Some(block("opus-5[1m]"));
        let mut b = a.clone();
        a.kind = "backlog-item".into();
        b.kind = "user-feedback".into();
        b.steps[0].kind = "answer-question".into();

        let found = agent_constraints_of(&[a.clone(), b.clone()]);
        assert_eq!(
            found.len(),
            1,
            "two protocols, one (role, model): {found:#?}"
        );
        let only = found.iter().next().expect("one");
        assert_eq!(agent_station_name(only), "a.platform-admin.opus-5-1m");

        let out = agent_stations(&[a.clone(), b.clone()], &[], now());
        assert_eq!(out.len(), 1, "{out:#?}");
        let s = &out[0];
        assert_eq!(s.name, "a.platform-admin.opus-5-1m");
        assert_eq!(s.kind, StationKind::Constraint);
        let step = s.predicate.step.as_ref().expect("matches a step");
        assert_eq!(
            step.kind, None,
            "any step kind: the model is the constraint"
        );
        assert_eq!(
            step.metadata_equals,
            BTreeMap::from([
                ("agent_model".to_string(), "opus-5[1m]".to_string()),
                ("authority_role".to_string(), "platform-admin".to_string()),
            ])
        );
        assert_eq!(step.status_in, vec![StepStatus::Ready, StepStatus::Active]);
        let cap = s.capability.as_ref().expect("gated");
        assert_eq!(cap.roles, vec!["platform-admin".to_string()]);
        assert_eq!(cap.models, vec!["opus-5[1m]".to_string()]);

        // A second model under the same role is a second station; the
        // whole projection carries both kinds of derived row.
        b.steps[0].agent = Some(block("haiku-4-5"));
        let names: Vec<String> = derived_stations(&[a.clone(), b.clone()], &[], now())
            .into_iter()
            .map(|s| s.name)
            .collect();
        assert!(
            names.contains(&"a.platform-admin.opus-5-1m".to_string()),
            "{names:?}"
        );
        assert!(
            names.contains(&"a.platform-admin.haiku-4-5".to_string()),
            "{names:?}"
        );
        assert!(
            names.contains(&"q.platform-admin.task".to_string()),
            "{names:?}"
        );

        // No role, no station: nothing to gate the claim on.
        a.steps[0].authority_role = None;
        assert!(agent_constraints_of(std::slice::from_ref(&a)).is_empty());
        // An authored name still wins.
        let taken = ["a.platform-admin.haiku-4-5".to_string()];
        assert!(
            !agent_stations(&[b], &taken, now())
                .iter()
                .any(|s| s.name == taken[0])
        );
    }

    /// The platform bundle declares agent blocks (car 1 put them on the
    /// backlog-item, user-feedback and retro steps), so the live
    /// projection carries at least one `(role, model)` station — the
    /// row the probe on this car reads back from `/api/stations`.
    #[test]
    fn the_platform_set_projects_an_agent_station_with_a_url_safe_name() {
        let derived = agent_stations(&seedable_platform_workflows(), &[], now());
        assert!(!derived.is_empty(), "the bundle declares agent blocks");
        for s in &derived {
            assert!(s.name.starts_with("a."), "{}", s.name);
            assert!(
                s.name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "-._".contains(c)),
                "`{}` needs percent-encoding to appear in a URL — the model is slugged",
                s.name
            );
            let cap = s.capability.as_ref().expect("gated");
            assert_eq!((cap.roles.len(), cap.models.len()), (1, 1), "{}", s.name);
            assert!(
                crate::agent_spec::known_models().contains(&cap.models[0]),
                "{}: {} is priced",
                s.name,
                cap.models[0]
            );
        }
        assert_eq!(model_slug("opus-5[1m]"), "opus-5-1m");
        assert_eq!(model_slug("Haiku 4.5"), "haiku-4-5");
    }
}
