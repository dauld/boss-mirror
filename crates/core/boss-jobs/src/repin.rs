//! WHAT A RE-PIN WRITES — the plan that makes a moved packet read the
//! version it was moved to (design 7cf202a9, answering backlog
//! 4347a1af).
//!
//! A packet stays on its admission version unless an actor explicitly
//! moves it, and the move is on the record. Before this module the door
//! (`POST /api/jobs/{id}/convert`) changed `jobs.workflow_version` and
//! nothing else, so it moved the envelope and not the text an executor
//! reads: a step row carries what materialisation copied onto it — its
//! `procedure` and the other `metadata_defaults`, its kind, its title,
//! its completion `fields` (which completion validates against, not the
//! spec), its sign-offs and assurance — and a step the target inserts
//! had no row at all. Measured on page-audit c0d2caf0 (pinned v1, active
//! v3, differing only in two pending procedures): the door would have
//! answered `converted: true` and changed nothing an executor reads
//! (backlog 1e973965). An interim car refused every such move; this is
//! the half that carries it.
//!
//! So a re-pin, per Q2:
//!
//! - RE-PROJECTS every step not yet finished (pending, ready, active)
//!   from the target version: the row columns the spec owns (kind,
//!   title, fields, sign-offs, assurance) and each projected metadata
//!   key the two versions disagree on.
//! - MATERIALISES every step the target inserts, pending, at its
//!   position; the caller's readiness pass promotes it like any other.
//! - LEAVES every completed (and skipped) step with the text it ran
//!   under. Only its `sort_order` may move, so the list stays in the
//!   target's order when a step is inserted ahead of it.
//!
//! A PROJECTED KEY SOMEONE HAS SINCE WRITTEN IS KEPT, AND NAMED. The
//! row's metadata is the projection PLUS whatever the executor wrote.
//! A key is replaced only while the row still holds exactly what the
//! admission version projected there; a key holding anything else is
//! the executor's, and overwriting it would destroy work on an active
//! step. It is kept, and the record lists it as `kept`, so a reader can
//! see the one place the moved packet still reads its old text rather
//! than discover it.
//!
//! Whether the move is SAFE is not decided here —
//! [`crate::protocol_conversion::convertibility_for_packet`] decides
//! that, and the door asks it first. This decides only what the move
//! writes. Everything here is pure; the handler does the I/O.

use crate::registry::{WorkflowSpec, materialize_steps_at};
use boss_core::job::{Job, Step, StepId, StepStatus};
use serde_json::{Map, Value, json};
use std::collections::{BTreeSet, HashMap};

/// The reserved job-metadata key the re-pin record is appended to.
/// Written only by the re-pin door; the generic metadata PATCH refuses
/// it by this name and the job PUT carries the stored list forward, so
/// the list only ever grows (Q3).
pub const REPINS_KEY: &str = "repins";

/// The refusal the generic job metadata PATCH answers a `repins` key
/// with, naming the one door that writes it.
pub const PATCH_REFUSAL: &str = "`repins` is a reserved, append-only list: it is written only \
     by the re-pin door, POST /api/jobs/{id}/convert (`boss job convert`), in the same \
     transaction that moves the packet. A metadata patch could rewrite or erase the record \
     of a move";

/// One step the re-pin rewrote: the row as it will be written, what
/// changed on it, and which projected keys it kept because someone had
/// written them.
#[derive(Debug, Clone, PartialEq)]
pub struct Reprojected {
    pub step: Step,
    /// What this re-pin changed on the row, named the way an operator
    /// looks for it: a metadata key in backticks, a column by name.
    pub changed: Vec<String>,
    /// Projected keys the target changes that this row no longer holds
    /// at their admission value, and so were left as written.
    pub kept: Vec<String>,
}

/// Everything one re-pin writes to the step rows.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct RepinPlan {
    pub reprojected: Vec<Reprojected>,
    pub inserted: Vec<Step>,
}

/// Why a packet cannot be planned at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unplannable(pub String);

/// Plan the move of `job` (its step rows `steps`) from `from` to `to`.
///
/// Both versions are projected with the SAME inputs the admission used
/// — the packet's subject, its metadata, and its opening day as the
/// `{day}` anchor — so the admission projection can be compared against
/// what the row actually holds, key by key.
///
/// Refused when a step row carries no `spec_slug`: such a packet
/// predates the column, its steps pair to the spec only by position,
/// and a plan keyed by slug would read every one of them as missing
/// and insert a duplicate.
pub fn plan(
    from: &WorkflowSpec,
    to: &WorkflowSpec,
    job: &Job,
    steps: &[Step],
) -> Result<RepinPlan, Unplannable> {
    if let Some(bare) = steps.iter().find(|s| s.spec_slug.is_none()) {
        return Err(Unplannable(format!(
            "step `{}` carries no spec slug — this packet predates the column, its steps \
             pair to the protocol only by position, and a re-pin keyed by slug would insert \
             a duplicate of every one of them",
            bare.title
        )));
    }
    let project = |spec: &WorkflowSpec| {
        materialize_steps_at(
            spec,
            &job.subject,
            job.id,
            &job.metadata,
            StepId::new,
            Some(job.opened_on),
            None,
        )
    };
    let admitted: HashMap<String, Step> = project(from)
        .into_iter()
        .filter_map(|s| s.spec_slug.clone().map(|slug| (slug, s)))
        .collect();
    let target = project(to);
    let on_packet: HashMap<&str, &Step> = steps
        .iter()
        .filter_map(|s| s.spec_slug.as_deref().map(|slug| (slug, s)))
        .collect();

    // The target projection's step ids are fresh; an edge in it points
    // at the packet's own row where the packet has one, and at the
    // inserted row where it does not.
    let id_for: HashMap<StepId, StepId> = target
        .iter()
        .map(|t| {
            let slug = t.spec_slug.as_deref().unwrap_or_default();
            (t.id, on_packet.get(slug).map_or(t.id, |row| row.id))
        })
        .collect();
    let edges = |t: &Step| -> Vec<StepId> {
        t.blocked_by
            .iter()
            .map(|id| id_for.get(id).copied().unwrap_or(*id))
            .collect()
    };

    let mut out = RepinPlan::default();
    for t in &target {
        let slug = t.spec_slug.as_deref().unwrap_or_default();
        let Some(row) = on_packet.get(slug) else {
            out.inserted.push(Step {
                status: StepStatus::Pending,
                blocked_by: edges(t),
                ..t.clone()
            });
            continue;
        };
        let mut next = (*row).clone();
        next.sort_order = t.sort_order;
        let mut kept = Vec::new();
        let live = !matches!(row.status, StepStatus::Completed | StepStatus::Skipped);
        if live && let Some(was) = admitted.get(slug) {
            next.kind = t.kind.clone();
            next.fields = t.fields.clone();
            next.sign_offs_required = t.sign_offs_required.clone();
            next.assurance_required = t.assurance_required;
            next.blocked_by = edges(t);
            if was.title != t.title {
                if row.title == was.title {
                    next.title = t.title.clone();
                } else {
                    kept.push("title".to_string());
                }
            }
            if was.assignee_id != t.assignee_id {
                if row.assignee_id == was.assignee_id {
                    next.assignee_id = t.assignee_id.clone();
                } else {
                    kept.push("assignee".to_string());
                }
            }
            let (metadata, kept_keys) =
                reproject_metadata(&row.metadata, &was.metadata, &t.metadata);
            next.metadata = metadata;
            kept.extend(kept_keys.into_iter().map(|k| format!("`{k}`")));
        }
        let changed = differences(row, &next);
        if !changed.is_empty() || !kept.is_empty() {
            out.reprojected.push(Reprojected {
                step: next,
                changed,
                kept,
            });
        }
    }
    Ok(out)
}

/// The row's metadata with every key the two projections disagree on
/// moved to the target's value — or removed, when the target projects
/// none — provided the row still holds the admission value there. The
/// second half of the answer names the keys it left alone for that
/// reason. A key both projections agree on is not the protocol's
/// change, and is never touched.
fn reproject_metadata(row: &Value, was: &Value, now: &Value) -> (Value, Vec<String>) {
    let empty = Map::new();
    let (was, now) = (
        was.as_object().unwrap_or(&empty),
        now.as_object().unwrap_or(&empty),
    );
    let mut md = row.as_object().cloned().unwrap_or_default();
    let mut kept = Vec::new();
    let keys: BTreeSet<&String> = was.keys().chain(now.keys()).collect();
    for k in keys {
        if was.get(k) == now.get(k) {
            continue;
        }
        if md.get(k) != was.get(k) {
            kept.push(k.clone());
            continue;
        }
        match now.get(k) {
            Some(v) => {
                md.insert(k.clone(), v.clone());
            }
            None => {
                md.remove(k);
            }
        }
    }
    (Value::Object(md), kept)
}

/// What differs between a row before and after, named the way an
/// operator looks for it.
fn differences(before: &Step, after: &Step) -> Vec<String> {
    let empty = Map::new();
    let (b, a) = (
        before.metadata.as_object().unwrap_or(&empty),
        after.metadata.as_object().unwrap_or(&empty),
    );
    let keys: BTreeSet<&String> = b.keys().chain(a.keys()).collect();
    let metadata = keys
        .into_iter()
        .filter(|k| b.get(*k) != a.get(*k))
        .map(|k| format!("`{k}`"));
    let columns = [
        (before.kind != after.kind, "kind"),
        (before.title != after.title, "title"),
        (before.fields != after.fields, "fields"),
        (
            before.sign_offs_required != after.sign_offs_required,
            "sign-offs",
        ),
        (
            before.assurance_required != after.assurance_required,
            "assurance",
        ),
        (before.assignee_id != after.assignee_id, "assignee"),
        (before.blocked_by != after.blocked_by, "blocked_by"),
        (before.sort_order != after.sort_order, "sort_order"),
    ]
    .into_iter()
    .filter(|(differs, _)| *differs)
    .map(|(_, name)| name.to_string());
    metadata.chain(columns).collect()
}

/// The record of one move — the entry appended to the packet's
/// [`REPINS_KEY`] list and the body of its `jobs.job.repinned` event
/// (Q3): from, to, who, when, each step re-projected with what changed
/// and what it kept, and each step inserted. A reader tells a packet
/// admitted at v3 from one moved there by this, without diffing
/// `job.updated` payloads.
pub fn record(
    plan: &RepinPlan,
    from: i32,
    to: i32,
    by: &str,
    at: chrono::DateTime<chrono::Utc>,
) -> Value {
    let (reprojected, inserted) = listed(plan);
    json!({
        "from": from,
        "to": to,
        "by": by,
        "at": at.to_rfc3339(),
        "reprojected": reprojected,
        "inserted": inserted,
    })
}

/// The plan's two lists as the record spells them — also what a dry
/// run answers, so the preview and the record cannot disagree.
pub fn listed(plan: &RepinPlan) -> (Value, Value) {
    let reprojected: Vec<Value> = plan
        .reprojected
        .iter()
        .map(|r| {
            let mut entry = json!({
                "step": r.step.spec_slug,
                "step_id": r.step.id.to_string(),
                "changed": r.changed,
            });
            if !r.kept.is_empty() {
                entry["kept"] = json!(r.kept);
            }
            entry
        })
        .collect();
    let inserted: Vec<Value> = plan
        .inserted
        .iter()
        .map(|s| json!({ "step": s.spec_slug, "step_id": s.id.to_string() }))
        .collect();
    (Value::Array(reprojected), Value::Array(inserted))
}

/// The packet's metadata with `entry` appended to its [`REPINS_KEY`]
/// list. A non-object metadata or a non-list under the key reads as
/// empty — the same fold the Postgres adapter's one UPDATE states.
pub fn appended(metadata: &Value, entry: &Value) -> Value {
    let mut md = metadata.as_object().cloned().unwrap_or_default();
    let mut list = md
        .get(REPINS_KEY)
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    list.push(entry.clone());
    md.insert(REPINS_KEY.to_string(), Value::Array(list));
    Value::Object(md)
}

/// The `jobs.job.repinned` payload: the record, plus the packet it
/// moved under `job_id` — the key every job-scoped marker uses.
pub fn repinned_payload(job_id: &str, entry: &Value) -> Value {
    let mut payload = entry.clone();
    if let Some(obj) = payload.as_object_mut() {
        obj.insert("job_id".to_string(), Value::String(job_id.to_string()));
    }
    payload
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::StepSpec;
    use boss_core::job::{JobId, JobStatus, Priority, StepField, Subject};

    fn step(title: &str) -> StepSpec {
        StepSpec {
            title: title.to_string(),
            kind: "task".to_string(),
            ready_when: "true".to_string(),
            terminal: None,
            title_template: String::new(),
            sign_offs_required: Vec::new(),
            assurance_required: None,
            duration_hours: None,
            labor_hours: None,
            wall_clock_hours: None,
            fields: Vec::new(),
            authority_role: None,
            claimable: None,
            audience: None,
            agent: None,
            metadata_defaults: json!({}),
        }
    }

    fn with_procedure(title: &str, procedure: &str) -> StepSpec {
        let mut s = step(title);
        s.metadata_defaults = json!({ "procedure": procedure });
        s
    }

    fn after(title: &str, prior: &str) -> StepSpec {
        let mut s = step(title);
        s.ready_when = format!("steps.{prior}.done");
        s
    }

    fn wf(version: i32, steps: Vec<StepSpec>) -> WorkflowSpec {
        let mut spec = WorkflowSpec::platform_seed(
            "page-audit",
            "Page audit",
            "governance",
            vec!["custom".to_string()],
            steps,
        );
        spec.version = version;
        spec
    }

    fn job() -> Job {
        Job {
            id: JobId::new(),
            kind: "page-audit".into(),
            workflow_version: 1,
            subject: Subject::new("custom", "/it"),
            title: "t".into(),
            owner_id: "emp-1".into(),
            status: JobStatus::Open,
            priority: Priority::Standard,
            opened_on: chrono::NaiveDate::from_ymd_opt(2026, 9, 1).unwrap(),
            opened_at: None,
            due_on: None,
            closed_on: None,
            metadata: json!({}),
            tags: vec![],
            partition: boss_core::partition::Partition::Real,
        }
    }

    /// The packet as admitted under `spec`, with `done` completed.
    fn admitted(spec: &WorkflowSpec, job: &Job, done: &[&str]) -> Vec<Step> {
        materialize_steps_at(
            spec,
            &job.subject,
            job.id,
            &job.metadata,
            StepId::new,
            Some(job.opened_on),
            None,
        )
        .into_iter()
        .map(|mut s| {
            if done.contains(&s.spec_slug.as_deref().unwrap_or_default()) {
                s.status = StepStatus::Completed;
            }
            s
        })
        .collect()
    }

    fn named<'a>(plan: &'a RepinPlan, slug: &str) -> &'a Reprojected {
        plan.reprojected
            .iter()
            .find(|r| r.step.spec_slug.as_deref() == Some(slug))
            .unwrap_or_else(|| panic!("{slug} re-projected: {plan:?}"))
    }

    /// THE MEASURED CASE (page-audit c0d2caf0): v1 -> v3 changes only
    /// the procedure of a step the packet has not reached. The pending
    /// row now reads v3; the completed one keeps what it ran under.
    #[test]
    fn a_pending_procedure_is_reprojected_and_a_completed_one_is_kept() {
        let j = job();
        let v1 = wf(
            1,
            vec![
                with_procedure("measure", "Measure it."),
                with_procedure("file", "File it."),
            ],
        );
        let v3 = wf(
            3,
            vec![
                with_procedure("measure", "Measure it, twice."),
                with_procedure("file", "File it, with the page."),
            ],
        );
        let rows = admitted(&v1, &j, &["measure"]);

        let p = plan(&v1, &v3, &j, &rows).expect("planned");
        let file = named(&p, "file");
        assert_eq!(file.step.metadata["procedure"], "File it, with the page.");
        assert_eq!(file.changed, vec!["`procedure`".to_string()]);
        assert!(
            p.reprojected
                .iter()
                .all(|r| r.step.spec_slug.as_deref() != Some("measure")),
            "a completed step keeps the text it ran under: {p:?}"
        );
        assert!(p.inserted.is_empty());
    }

    /// A key the executor has written since admission is theirs: the
    /// target's change to it is not applied, and the record says so.
    #[test]
    fn a_projected_key_someone_has_written_is_kept_and_named() {
        let j = job();
        let v1 = wf(1, vec![with_procedure("file", "File it.")]);
        let v2 = wf(2, vec![with_procedure("file", "File it, with the page.")]);
        let mut rows = admitted(&v1, &j, &[]);
        rows[0].status = StepStatus::Active;
        rows[0].metadata["procedure"] = json!("File it (my notes).");

        let p = plan(&v1, &v2, &j, &rows).expect("planned");
        let file = named(&p, "file");
        assert_eq!(file.step.metadata["procedure"], "File it (my notes).");
        assert_eq!(file.kept, vec!["`procedure`".to_string()]);
        assert!(file.changed.is_empty(), "{file:?}");
    }

    /// A key the executor wrote that no projection names is simply
    /// carried: the re-pin touches only what the protocol changed.
    #[test]
    fn evidence_the_executor_wrote_rides_through_untouched() {
        let j = job();
        let v1 = wf(1, vec![with_procedure("file", "File it.")]);
        let v2 = wf(2, vec![with_procedure("file", "File it, with the page.")]);
        let mut rows = admitted(&v1, &j, &[]);
        rows[0].metadata["evidence"] = json!("half done");

        let p = plan(&v1, &v2, &j, &rows).expect("planned");
        assert_eq!(named(&p, "file").step.metadata["evidence"], "half done");
    }

    /// The row's completion contract moves with it: completion
    /// validates `step.fields`, so a required field the target adds to
    /// a pending step is only "simply collected" once the row has it.
    #[test]
    fn the_row_columns_the_spec_owns_are_reprojected() {
        let j = job();
        let v1 = wf(1, vec![step("scope"), after("build", "scope")]);
        let mut build = after("build", "scope");
        build.fields = vec![StepField {
            name: "test".into(),
            field_type: "string".into(),
            required: true,
            filled_by: boss_core::job::FilledBy::Executor,
            item_keys: Vec::new(),
            covers: None,
            binds: None,
            item_value_max_bytes: None,
        }];
        build.kind = "checklist".into();
        build.sign_offs_required = vec!["cto".into()];
        build.title_template = "Build the change".into();
        let v2 = wf(2, vec![step("scope"), build]);
        let rows = admitted(&v1, &j, &["scope"]);

        let p = plan(&v1, &v2, &j, &rows).expect("planned");
        let b = named(&p, "build");
        assert_eq!(b.step.fields.len(), 1);
        assert_eq!(b.step.kind, "checklist");
        assert_eq!(b.step.sign_offs_required, vec!["cto".to_string()]);
        assert_eq!(b.step.title, "Build the change");
        for name in ["fields", "kind", "sign-offs", "title"] {
            assert!(b.changed.iter().any(|c| c == name), "{name}: {b:?}");
        }
        assert_eq!(b.step.id, rows[1].id, "the packet's own row, rewritten");
    }

    /// A step the target inserts gets a row: pending, at its position,
    /// its edge pointing at the packet's own upstream row — and the
    /// step it was inserted ahead of moves down one place.
    #[test]
    fn an_inserted_step_is_materialised_at_its_position() {
        let j = job();
        let v1 = wf(1, vec![step("triage"), after("build", "triage")]);
        let v2 = wf(
            2,
            vec![
                step("triage"),
                after("draft-design", "triage"),
                after("build", "triage"),
            ],
        );
        let rows = admitted(&v1, &j, &["triage"]);

        let p = plan(&v1, &v2, &j, &rows).expect("planned");
        assert_eq!(p.inserted.len(), 1, "{p:?}");
        let ins = &p.inserted[0];
        assert_eq!(ins.spec_slug.as_deref(), Some("draft-design"));
        assert_eq!(ins.status, StepStatus::Pending);
        assert_eq!(ins.sort_order, 1);
        assert_eq!(ins.job_id, j.id);
        assert_eq!(
            ins.blocked_by,
            vec![rows[0].id],
            "edge to the packet's triage row"
        );
        let build = named(&p, "build");
        assert_eq!(build.step.sort_order, 2);
        assert_eq!(build.changed, vec!["sort_order".to_string()]);
    }

    /// Nothing differs, nothing is written: a move between two versions
    /// that agree on every step re-projects no row.
    #[test]
    fn an_identical_step_set_reprojects_nothing() {
        let j = job();
        let v1 = wf(1, vec![with_procedure("file", "File it.")]);
        let v2 = wf(2, vec![with_procedure("file", "File it.")]);
        let rows = admitted(&v1, &j, &[]);
        assert_eq!(plan(&v1, &v2, &j, &rows), Ok(RepinPlan::default()));
    }

    /// A packet whose rows predate `spec_slug` cannot be paired by name.
    #[test]
    fn a_packet_without_slugs_is_refused_rather_than_duplicated() {
        let j = job();
        let v1 = wf(1, vec![step("file")]);
        let mut rows = admitted(&v1, &j, &[]);
        rows[0].spec_slug = None;
        let why = plan(&v1, &wf(2, vec![step("file")]), &j, &rows).expect_err("refused");
        assert!(why.0.contains("spec slug"), "{why:?}");
    }

    /// The record says what moved and to where, so a reader can tell a
    /// moved packet from one admitted there; and the list only grows.
    #[test]
    fn the_record_is_appended_and_names_each_step() {
        let j = job();
        let v1 = wf(1, vec![step("triage"), with_procedure("file", "a")]);
        let v2 = wf(
            2,
            vec![
                step("triage"),
                with_procedure("file", "b"),
                after("archived", "file"),
            ],
        );
        let p = plan(&v1, &v2, &j, &admitted(&v1, &j, &["triage"])).expect("planned");
        let at = chrono::DateTime::<chrono::Utc>::MIN_UTC;
        let entry = record(&p, 1, 2, "emp-bootstrap-admin", at);
        assert_eq!(entry["from"], 1);
        assert_eq!(entry["to"], 2);
        assert_eq!(entry["by"], "emp-bootstrap-admin");
        assert_eq!(entry["reprojected"][0]["step"], "file");
        assert_eq!(entry["reprojected"][0]["changed"][0], "`procedure`");
        assert_eq!(entry["inserted"][0]["step"], "archived");

        let once = appended(&json!({"other": 1}), &entry);
        let twice = appended(&once, &entry);
        assert_eq!(twice[REPINS_KEY].as_array().map(Vec::len), Some(2));
        assert_eq!(twice["other"], 1);
        assert_eq!(
            repinned_payload("abc", &entry)["job_id"],
            "abc",
            "the marker names its packet"
        );
    }
}
