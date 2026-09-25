//! Shared types for the Job coordination primitive.
//!
//! A Job is a bounded unit of coordinated work — device repair, procurement,
//! sales cycle, marketing campaign, employee onboarding. Jobs decompose into
//! Steps, each owned by a person or team, with optional sign-off gates
//! and cross-job dependency tracking.
//!
//! These types live in boss-core because they are shared vocabulary: every
//! domain crate can reference a JobId or Subject without depending on the
//! boss-jobs service crate.

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

use crate::define_id;
use crate::partition::Partition;

// ---------------------------------------------------------------------------
// Identity
// ---------------------------------------------------------------------------

define_id!(JobId);
define_id!(StepId);

// ---------------------------------------------------------------------------
// Enums
// ---------------------------------------------------------------------------

/// What the Job is about — a (kind, id) tuple referencing any
/// identity-bearing entity in the system.
///
/// Wire shape: `{"subject_kind": "<kind>", "id": "..."}`.
///
/// Per the founding-invariant that taxonomies are data, Subject is a
/// (kind, id) pair validated against the boss-subject-kinds registry
/// — not a closed Rust enum. Tenants add a new SubjectKind by
/// registering a row, never by forking core.
///
/// Core owns only the **mechanism** — the (kind, id) shape, `new`,
/// and the kind/id accessors. The **vocabulary** (the five root noun
/// axes and every specialization — `asset`, `account`, `vendor`,
/// `purchase_order`, …) lives in the SubjectKind registry as data,
/// the single source of truth. Tier-1 core names no business noun:
/// callers build kinds with `Subject::new("<kind>", id)`, the kind
/// string being the data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Subject {
    #[serde(rename = "subject_kind")]
    pub kind: String,
    pub id: String,
}

impl crate::primitives::Subject for Subject {
    fn kind(&self) -> &str {
        &self.kind
    }
    fn id(&self) -> &str {
        &self.id
    }
}

impl Subject {
    /// Build a Subject of any kind. The kind is data — a string
    /// validated against the SubjectKind registry at write time, not a
    /// closed set core enumerates. There are deliberately no
    /// per-kind constructors: core names no business noun.
    pub fn new(kind: impl Into<String>, id: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            id: id.into(),
        }
    }
}

/// Lifecycle status of a Job.
///
/// Four words: `draft` and `open` are live, `closed` and `cancelled`
/// are terminal. `Blocked` and `PendingSignOff` were retired on
/// 2026-09-24 (backlog 3c3dc8f3): nothing ever set either — a step
/// paused on a dependency is a `Pending` step on an `Open` Job, and
/// completion refuses an unsigned step — and on the day they went,
/// 0 of 16,963 Jobs and 0 of 97,909 `jobs.job.*` audit events (the
/// whole log, 2026-09-16 onward) carried either word. What they did
/// do was render as two status filters that could only ever answer
/// "No jobs match." A retired word now fails to deserialize, and the
/// `jobs.status` CHECK refuses it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum JobStatus {
    Draft,
    Open,
    Closed,
    Cancelled,
}

/// Job priority.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Priority {
    Emergency,
    Urgent,
    Standard,
    Scheduled,
}

/// Status of a single Step line item.
///
/// Five statuses describing one predicate-driven lifecycle. Each
/// Step carries a `ready_when` predicate (declared on its Workflow
/// `StepSpec`); the materializer evaluates it at Job open and the
/// re-evaluator re-checks it on every upstream change. Status is the
/// program counter — which nodes of the Workflow's implicit DAG have
/// fired, which are eligible, which the predicates ruled out.
///
/// ```text
///   pending ─(ready_when true)─▶ ready ─(assigned)─▶ active ─(complete)─▶ completed
///      │
///      └─(ready_when provably false-forever)─▶ skipped
/// ```
///
/// No `Blocked` / `Aborted`: a step paused on a dependency is
/// simply `Pending` until its predicate flips, and an abandoned
/// branch is `Skipped`. Reactions to external state live in
/// dispatcher rules, not in the step lifecycle. See
/// docs/architecture-decisions.md §Jobs, Workflows, Steps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum StepStatus {
    /// `ready_when` evaluates false — planned but not yet eligible.
    /// Rendered greyed out.
    #[default]
    Pending,
    /// `ready_when` evaluates true — eligible, awaiting an executor
    /// to pick up the work.
    Ready,
    /// An executor picked up the step; work is in flight.
    Active,
    /// Done successfully. Metadata is committed; downstream
    /// predicates may now re-evaluate. Reaching `Completed` on a
    /// *terminal* step closes the Job with that step's outcome.
    Completed,
    /// `ready_when` is provably unsatisfiable (every upstream step it
    /// references reached a terminal state and the predicate is still
    /// false), or a terminal closed the Job before this step ran.
    /// Rendered struck-through — "not applicable."
    Skipped,
}

// ---------------------------------------------------------------------------
// Aggregates
// ---------------------------------------------------------------------------

/// The coordination envelope — one bounded unit of work.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Job {
    #[serde(default)]
    pub id: JobId,
    pub kind: String,
    /// Workflow version this Job opened under. Server-assigned at
    /// creation to the kind's active version — creation is blocked
    /// against draft/retired kinds, so this is the latest version at
    /// open time. In-flight Jobs stay pinned to it across later
    /// publishes. Default 1 keeps pre-versioning events replaying
    /// clean. Per docs/architecture-decisions.md §Jobs, Workflows, Steps.
    #[serde(default = "default_workflow_version")]
    pub workflow_version: i32,
    pub subject: Subject,
    pub title: String,
    pub owner_id: String,
    pub status: JobStatus,
    pub priority: Priority,
    pub opened_on: NaiveDate,
    /// The instant the packet was admitted — server-stamped at
    /// `POST /api/jobs`, immutable afterwards (the adapters keep it
    /// out of every UPDATE, the same way they keep `partition` out).
    ///
    /// `opened_on` is a DATE, so the finest honest answer it supports
    /// is a whole day; every surface that asks "how long has this been
    /// waiting" — the ops-runner's `oldest_wait_s`, dock wait and gate
    /// duration on the region map, the overdue alarms, the silence
    /// sweep — needs the instant. Until backlog 6c2eba00 that instant
    /// was `metadata.opened_at`, written by whoever filed the packet:
    /// a convention the doors happen to follow, absent on anything
    /// filed by a caller that does not know it. The metadata stamp is
    /// still written for the readers already on it; this is the field
    /// that cannot be absent by accident.
    ///
    /// `None` on packets that predate the column and whose
    /// `jobs.job.created` event the back-fill could not find. An
    /// absent stamp is the honest answer there — a projection of the
    /// log, never an invention (design f2cdff23, question `backfill`).
    #[serde(default)]
    pub opened_at: Option<chrono::DateTime<chrono::Utc>>,
    pub due_on: Option<NaiveDate>,
    pub closed_on: Option<NaiveDate>,
    pub metadata: serde_json::Value,
    pub tags: Vec<String>,
    /// Which company this Job belongs to — real, simulated, or shadow
    /// ([`Partition`]). Decided ONCE at admission (`POST /api/jobs`) —
    /// from an explicit body value or the sim-chain origin of the
    /// creating request — and immutable thereafter: a real operator
    /// can click around a simulated Job all day without making it
    /// real. Every event about the Job (job + step state events and
    /// markers) inherits this as its `_partition` / `_simulated`
    /// payload markers, so the value on the packet — not the
    /// transport context of any later write — is the source of truth.
    ///
    /// On the wire this is TWO keys (`partition` + the legacy
    /// `simulated`, derived as not-real) and reads either, so
    /// pre-flag payloads (old audit_log events, old clients)
    /// deserialize as real and an N-1 client that only knows the bool
    /// still fails closed on a shadow packet. See `partition::wire`.
    #[serde(flatten, with = "crate::partition::wire")]
    pub partition: Partition,
}

impl Job {
    pub fn new(
        kind: impl Into<String>,
        subject: Subject,
        title: impl Into<String>,
        owner_id: impl Into<String>,
        priority: Priority,
        opened_on: NaiveDate,
    ) -> Self {
        Self {
            id: JobId::new(),
            kind: kind.into(),
            workflow_version: default_workflow_version(),
            subject,
            title: title.into(),
            owner_id: owner_id.into(),
            status: JobStatus::Draft,
            priority,
            opened_on,
            // Server-stamped at admission, not by a constructor: a
            // `Job::new` in a sim or replay path has no admission
            // instant to report, and inventing one here would be the
            // habit this field replaces.
            opened_at: None,
            due_on: None,
            closed_on: None,
            metadata: serde_json::Value::Object(serde_json::Map::new()),
            tags: Vec::new(),
            partition: Partition::Real,
        }
    }

    pub fn with_due_on(mut self, due_on: NaiveDate) -> Self {
        self.due_on = Some(due_on);
        self
    }

    /// Override the auto-generated `JobId` with one supplied by
    /// the caller. Used by sim/replay paths that derive IDs from
    /// a seeded RNG so two runs with identical inputs produce
    /// byte-identical output (correctness-protocol property 5).
    /// Production callers that don't need replay determinism
    /// stay on the `Job::new` default.
    pub fn with_id(mut self, id: JobId) -> Self {
        self.id = id;
        self
    }

    /// Set the Workflow version this Job opened under. Production goes
    /// through the create handler, which stamps the kind's active
    /// version; this builder is for sim/replay/test paths that
    /// construct a Job directly.
    pub fn with_workflow_version(mut self, version: i32) -> Self {
        self.workflow_version = version;
        self
    }

    pub fn with_metadata(mut self, metadata: serde_json::Value) -> Self {
        self.metadata = metadata;
        self
    }

    pub fn with_tags(mut self, tags: Vec<String>) -> Self {
        self.tags = tags;
        self
    }

    /// Fix the Job's partition at construction. Sim engines set
    /// `Simulated` so admission fixes the value from the packet
    /// itself, not from per-event stamping.
    pub fn with_partition(mut self, partition: Partition) -> Self {
        self.partition = partition;
        self
    }
}

/// A step-authored completion-contract field (inline authoring —
/// architecture-decisions.md §Step types are property bundles):
/// the same schema language registry bundles carry,
/// declared per step in the Workflow. Validation is the union of the
/// bundle's fields and these.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepField {
    pub name: String,
    pub field_type: String,
    #[serde(default)]
    pub required: bool,
    /// Which party supplies this field's value. `serde(default)` so
    /// every field authored before this existed reads as `Executor` —
    /// required-at-done, exactly the contract it already had.
    #[serde(default)]
    pub filled_by: FilledBy,
    /// For an `array` field: the keys every element must carry, each
    /// with a non-empty string value. Registry data, so a protocol can
    /// state the SHAPE of its structured input and not only its
    /// presence — a design doc's `questions` are `[{anchor, title,
    /// proposal}]`, and eight packets in one session (2026-09-04/05)
    /// were admitted with no `title` on any element and dead-ended at
    /// "nothing to review". Empty (the default) means any element
    /// shape is accepted, which is what every field authored before
    /// this existed already meant.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub item_keys: Vec<String>,
    /// For an `array` field of `{anchor, …}` elements: the name of
    /// another array field on the same step whose every element's
    /// `anchor` must appear as the `anchor` of one element here, checked
    /// required-at-done. Registry data for a cross-field contract: a
    /// design doc's `resolutions` COVER its `questions`, so the review
    /// cannot complete with a question unanswered. Measured 2026-09-11
    /// (0ef658e6): four design packets reached `fold` with their review
    /// completed and ZERO resolutions against 2, 3, 3 and 4 questions —
    /// twelve judgements recorded nowhere — because `resolutions` was
    /// not a declared field and nothing related it to `questions`. None
    /// (the default) means no coverage contract, which is what every
    /// field authored before this existed already meant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub covers: Option<String>,
    /// For an `array` field of `{anchor, …}` elements: the name of
    /// another array field on the same step whose anchors an element
    /// here may BIND, by carrying a key of that same name holding a list
    /// of them. Checked at every write that touches either field (the
    /// step merge door) and again at done: a bound anchor the other
    /// field does not carry is refused, naming it. Registry data for the
    /// relation design 26a89f11 decided — a design question binds the
    /// exhibits it is asked about (`questions` binds `exhibits`), and a
    /// binding to an exhibit nobody attached would render as a question
    /// pointing at nothing. None (the default) means no binding, which is
    /// what every field authored before this existed already meant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binds: Option<String>,
    /// For an `array` field: the most UTF-8 bytes any one STRING value of
    /// an element may hold, checked at every write that touches the field
    /// and again at done. A design exhibit's `html` rides inline in step
    /// metadata up to a bound (design 26a89f11: 256 KB), and a bound
    /// stated nowhere is a bound nobody holds. It measures the string, not
    /// its JSON encoding, so the number an author reads off `ls -l` is the
    /// number the refusal names. None (the default) means unbounded, which
    /// is what every field authored before this existed already meant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item_value_max_bytes: Option<u64>,
    /// For an `array` field: keys of which every element must carry
    /// EXACTLY ONE, as a non-empty string — the "one of" twin of
    /// `item_keys`, which names keys an element carries ALL of. Carrying
    /// two is refused at every write that touches the field (a record
    /// that says two things about one element is ambiguous whoever reads
    /// it); carrying none is refused at done, the way a missing item key
    /// is. Registry data for design 26a89f11's second arm: an exhibit is
    /// `{anchor, title}` plus its bytes inline as `html` OR, over the
    /// inline bound, a `file_ref` into the file store — never both, and
    /// never neither. Empty (the default) means no such choice, which is
    /// what every field authored before this existed already meant.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub item_one_of: Vec<String>,
}

/// Who supplies a step field's value — the enforcement point follows
/// the party who can actually fix an omission.
///
/// Required-at-done is correct for fields the EXECUTOR fills while
/// doing the work (`scheduled_at`, `decision`): they cannot exist at
/// create, so validating them there would be wrong. It is wrong for
/// fields the FILER must supply for the work to be doable at all (a
/// design review's `markdown`): deferring those to completion detonates
/// the 400 on the executor mid-work — the party least able to fix it.
/// A `Filer` field is validated at admission (job create), where the
/// filer is still on the line; completion validation is unchanged.
///
/// Protocol data, not a code path (§9): which fields a step's filer
/// owes is declared on the Workflow row (`filled_by = "filer"` in the
/// bundle TOML), never matched on kind in core. The default keeps
/// every existing spec byte-for-byte unchanged in meaning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum FilledBy {
    /// Filled in during the work; validated when the step completes.
    /// Today's behaviour, and the default so every field authored
    /// before this existed keeps its current meaning.
    #[default]
    Executor,
    /// Supplied by whoever files the Job; validated at admission so a
    /// missing value refuses the packet before anyone claims it.
    Filer,
}

/// One sign-off stamp (architecture-decisions.md §Step types are
/// property bundles): an authenticated authority's
/// attestation of a step *in its current shape*. Policy-checked at
/// stamp time and recorded as its own audit event; `shape_hash`
/// binds the stamp to the content it attested.
/// How hard a stamp was to produce.
///
/// A stamp already answers WHO attested and WHAT they attested
/// (`shape_hash`). It never answered how strong the evidence was, so
/// "David clicked approve" and "David logged in this morning and
/// something clicked approve" were the same fact.
///
/// David, 2026-08-16: *"Passkey authorization as actor-auth feature
/// for job packets is broadly useful. Let's make sure we design and
/// build it that way."* — so this is a property of a STAMP, not a
/// feature of one workflow. Elevation, a payment release, a deploy
/// sign-off and an incident's closure all want it and none should
/// invent it.
///
/// ORDERED, and the order is the whole contract: a step declares the
/// minimum it will accept and the endpoint refuses anything weaker.
/// Adding a variant means deciding where it sits on that scale.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum Assurance {
    /// An authenticated session said yes. Today's behaviour, and the
    /// default so that every existing stamp and every existing
    /// protocol keeps its current meaning.
    #[default]
    Session,
    /// A fresh WebAuthn assertion proved the actor was present, over a
    /// challenge bound to this step's `shape_hash`. The signature is
    /// itself the binding: it cannot be replayed against a different
    /// step (different challenge) or against an edited one (the hash
    /// moved).
    Presence,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SignOffStamp {
    /// Who stamped — an employee id (stamping is a human act; the
    /// policy-gated external-override path records its own stamp).
    pub authority_id: String,
    /// The required-role this stamp satisfies.
    pub role: String,
    pub stamped_at: chrono::DateTime<chrono::Utc>,
    /// `step_shape_hash` of the step when stamped.
    pub shape_hash: String,
    /// How strong the evidence was. `serde(default)` so every stamp
    /// written before this field existed reads as `Session`, which is
    /// exactly what it was.
    #[serde(default)]
    pub assurance: Assurance,
    /// Presence stamps only: the server nonce folded into the WebAuthn
    /// challenge (`sha256(shape_hash || ":" || nonce)`). Recorded so a
    /// stamp names the exact single-use challenge that produced it —
    /// the audit trail from stamp back to ceremony. Absent on Session
    /// stamps and on every stamp written before presence existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presence_nonce: Option<String>,
}

/// Hash of a step's completion-relevant content — what a sign-off
/// stamp attests. Title + metadata, canonically serialized (sorted
/// keys) so hashing is insertion-order independent. Fields that
/// don't change what is being agreed to (status, assignee, sort
/// order, plugin pin) are deliberately excluded.
///
/// A KEY IS WRITTEN JSON-ENCODED, as a value is (security review of
/// backlog fd7090cc, 2026-09-24). It was written raw, so a key carrying
/// `:` and `,` could spell its neighbours — `{"zz":1,"zzz":2}` and
/// `{"zz:1,zzz":2}` hashed alike, and a stamp over one shape verified on
/// the other. An encoded key ends where its closing quote does.
/// infra/ops/ops-runner.sh computes this same hash in jq and is pinned
/// equal to it by ops_runner_approval_sh.rs (CLAUDE.md §9a). Changing
/// the form re-hashes every step, so a stamp taken on a still-open step
/// before this landed no longer matches and must be taken again. The
/// server never re-judges a completed step's stamps; the ops-runner
/// does, on an approve step, but its approvals expire in ten minutes.
pub fn step_shape_hash(title: &str, metadata: &serde_json::Value) -> String {
    use sha2::{Digest, Sha256};
    fn canonical(v: &serde_json::Value, out: &mut Vec<u8>) {
        match v {
            serde_json::Value::Object(m) => {
                let mut keys: Vec<_> = m.keys().collect();
                keys.sort();
                out.push(b'{');
                for k in keys {
                    let encoded = serde_json::Value::String(k.clone()).to_string();
                    out.extend_from_slice(encoded.as_bytes());
                    out.push(b':');
                    canonical(&m[k], out);
                    out.push(b',');
                }
                out.push(b'}');
            }
            serde_json::Value::Array(a) => {
                out.push(b'[');
                for x in a {
                    canonical(x, out);
                    out.push(b',');
                }
                out.push(b']');
            }
            other => out.extend_from_slice(other.to_string().as_bytes()),
        }
    }
    let mut buf = Vec::new();
    buf.extend_from_slice(title.as_bytes());
    buf.push(0);
    canonical(metadata, &mut buf);
    let mut h = Sha256::new();
    h.update(&buf);
    hex::encode(h.finalize())
}

/// A line item on a Job — one typed unit of work that must be completed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Step {
    #[serde(default)]
    pub id: StepId,
    #[serde(default)]
    pub job_id: JobId,
    #[serde(default = "default_step_kind")]
    pub kind: String,
    pub title: String,
    /// The spec slug this step materialized from — the stable
    /// machine-facing identifier ("build", "gate"), distinct from
    /// `title`, which is rendered display text. Machine callers
    /// (boss-step.sh, deploy scripts) address steps by this; `None`
    /// on steps created outside a Workflow spec or before the column
    /// existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spec_slug: Option<String>,
    #[serde(default)]
    pub assignee_id: Option<String>,
    #[serde(default)]
    pub status: StepStatus,
    #[serde(default)]
    pub sort_order: i32,
    /// Upstream steps this step's `ready_when` predicate references,
    /// derived at materialization. Not the gate — the predicate is —
    /// but a denormalized edge list the SPA renders the DAG from and
    /// the re-evaluator keys its dependency index on.
    #[serde(default)]
    pub blocked_by: Vec<StepId>,
    /// Role codes that must each stamp this step in its current shape
    /// before it may complete (the sign-off contract). Materialized from the
    /// step type's bundle (with `@authority_role` resolved to this
    /// step's authority_role). Empty = no sign-offs required.
    #[serde(default)]
    pub sign_offs_required: Vec<String>,
    /// The weakest stamp this step will accept. `None` means "whatever
    /// the StepType's floor says", which is `Session` unless the kind
    /// raises it — so an unset field changes nothing.
    ///
    /// Declared on the step rather than only on the kind because two
    /// `sign-off` steps can want different strengths depending on what
    /// they gate: the same kind approves a stationery order and a
    /// production deploy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assurance_required: Option<Assurance>,
    /// Step-authored completion-contract fields (inline authoring): validated
    /// at completion in union with the step type's bundle fields.
    #[serde(default)]
    pub fields: Vec<StepField>,
    /// Stamps collected so far. A stamp attests the step *in the
    /// shape it had when stamped* (`shape_hash`); completion counts
    /// only stamps whose hash matches the current shape. Stale stamps
    /// stay recorded — they are provenance, not validity.
    #[serde(default)]
    pub sign_offs: Vec<SignOffStamp>,
    #[serde(default)]
    pub completed_on: Option<NaiveDate>,
    /// Who completed the step: the actor the API signed the completing
    /// write with. Server-stamped at the flip to `completed`, never
    /// client-supplied — a body's value is overwritten — and frozen
    /// with the row afterwards. A human session and a machine actor are
    /// told apart by the id itself (`emp-…` vs `automation:…` vs
    /// `<mode>:<model>`), which is the whole point: a decision a person
    /// made must be distinguishable from a rule copying a field after
    /// the fact (c17871fe). `None` on steps that predate the stamp or
    /// have not completed.
    #[serde(default)]
    pub completed_by: Option<crate::actor::ActorId>,
    /// The instant the step completed, from the same clock that dates
    /// `completed_on`. Same ownership as `completed_by`.
    #[serde(default)]
    pub completed_at: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default = "default_metadata")]
    pub metadata: serde_json::Value,
    #[serde(default)]
    pub notes: Option<String>,
    /// Plugin version snapshotted at step creation time. Zero means
    /// "no plugin" (the step renders through an in-tree surface) or
    /// "snapshot not taken yet". The writer sets it once on insert by
    /// looking up the active plugin for the step's kind; republishing
    /// the plugin later doesn't retroactively change which bundle a
    /// long-running job's step is pinned against.
    #[serde(default)]
    pub step_plugin_version: i32,
    /// Optional pointer to a child Job when this Step's work
    /// decomposes further. `Some(id)` means "traversal into this
    /// Step descends into that Job's step graph" — the structural
    /// realization of Step-as-Job recursion. Default
    /// `None` for ordinary steps.
    #[serde(default)]
    pub embedded_job: Option<JobId>,
}

fn default_workflow_version() -> i32 {
    1
}

fn default_step_kind() -> String {
    "generic".to_string()
}

fn default_metadata() -> serde_json::Value {
    serde_json::Value::Object(serde_json::Map::new())
}

impl Step {
    pub fn new(
        job_id: JobId,
        kind: impl Into<String>,
        title: impl Into<String>,
        sort_order: i32,
    ) -> Self {
        Self {
            id: StepId::new(),
            job_id,
            kind: kind.into(),
            title: title.into(),
            spec_slug: None,
            assignee_id: None,
            // New Steps default to Pending (ready_when not yet
            // satisfied). The re-evaluator promotes Pending → Ready
            // when the step's predicate first evaluates true. See the
            // `StepStatus` doc comment.
            status: StepStatus::Pending,
            sort_order,
            blocked_by: Vec::new(),
            sign_offs_required: Vec::new(),
            assurance_required: None,
            sign_offs: Vec::new(),
            fields: Vec::new(),
            completed_on: None,
            completed_by: None,
            completed_at: None,
            metadata: serde_json::Value::Object(serde_json::Map::new()),
            notes: None,
            step_plugin_version: 0,
            embedded_job: None,
        }
    }

    pub fn with_assignee(mut self, assignee_id: impl Into<String>) -> Self {
        self.assignee_id = Some(assignee_id.into());
        self
    }

    pub fn with_sign_offs_required(mut self, roles: Vec<String>) -> Self {
        self.sign_offs_required = roles;
        self
    }

    /// True when every required role has a stamp attesting the step's
    /// *current* shape. Stale stamps (collected before a
    /// later edit) don't count.
    pub fn sign_offs_satisfied(&self) -> bool {
        if self.sign_offs_required.is_empty() {
            return true;
        }
        let current = step_shape_hash(&self.title, &self.metadata);
        self.sign_offs_required.iter().all(|role| {
            self.sign_offs
                .iter()
                .any(|st| &st.role == role && st.shape_hash == current)
        })
    }

    pub fn with_blocked_by(mut self, blocked_by: Vec<StepId>) -> Self {
        self.blocked_by = blocked_by;
        self
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn job_id_unique() {
        let a = JobId::new();
        let b = JobId::new();
        assert_ne!(a, b);
    }

    #[test]
    fn step_id_unique() {
        let a = StepId::new();
        let b = StepId::new();
        assert_ne!(a, b);
    }

    #[test]
    fn job_serde_round_trip() {
        let job = Job::new(
            "test-kind",
            Subject::new("asset", "sys-001"),
            "Test job",
            "emp-42",
            Priority::Standard,
            NaiveDate::from_ymd_opt(2026, 4, 16).unwrap(),
        );
        let json = serde_json::to_string(&job).unwrap();
        let back: Job = serde_json::from_str(&json).unwrap();
        assert_eq!(job, back);
    }

    /// `opened_at` is a FIELD, not a filer habit (backlog 6c2eba00,
    /// design f2cdff23). It is server-stamped at admission, so
    /// `Job::new` leaves it absent; and it is `#[serde(default)]`, so
    /// every `jobs.job.created` payload written before the promotion
    /// still deserializes — a rebuild over an old slice must not fail,
    /// and must not invent an instant nobody observed.
    #[test]
    fn job_opened_at_is_absent_until_something_stamps_it() {
        let mut job = Job::new(
            "test-kind",
            Subject::new("asset", "sys-001"),
            "Test job",
            "emp-42",
            Priority::Standard,
            NaiveDate::from_ymd_opt(2026, 4, 16).unwrap(),
        );
        assert_eq!(job.opened_at, None, "Job::new must not invent an instant");

        let at = "2026-09-20T17:40:16.732718729Z"
            .parse::<chrono::DateTime<chrono::Utc>>()
            .unwrap();
        job.opened_at = Some(at);
        let v = serde_json::to_value(&job).unwrap();
        assert!(v.get("opened_at").is_some(), "the stamp rides the wire");
        let back: Job = serde_json::from_value(v.clone()).unwrap();
        assert_eq!(back.opened_at, Some(at), "sub-second resolution survives");

        let mut pre = v;
        pre.as_object_mut().unwrap().remove("opened_at");
        let back: Job = serde_json::from_value(pre).unwrap();
        assert_eq!(back.opened_at, None);
    }

    #[test]
    fn job_partition_defaults_real_for_pre_flag_payloads() {
        // Old audit_log payloads (and old clients) predate both the
        // `simulated` field and the `partition` one. The wire reader
        // must admit them as real Jobs — a rebuild over a pre-flag
        // slice must not fail, and must not invent a simulated
        // company.
        let job = Job::new(
            "test-kind",
            Subject::new("asset", "sys-001"),
            "Test job",
            "emp-42",
            Priority::Standard,
            NaiveDate::from_ymd_opt(2026, 4, 16).unwrap(),
        );
        let mut v = serde_json::to_value(&job).unwrap();
        v.as_object_mut().unwrap().remove("simulated");
        v.as_object_mut().unwrap().remove("partition");
        let back: Job = serde_json::from_value(v).unwrap();
        assert_eq!(back.partition, Partition::Real);

        // And the builder fixes it at construction for sim engines.
        assert_eq!(
            job.with_partition(Partition::Simulated).partition,
            Partition::Simulated
        );
    }

    #[test]
    fn job_wire_carries_partition_and_the_derived_legacy_bool() {
        // Expand/contract (packet 508cc38c): `simulated` stays on the
        // wire for N-1 readers, derived as not-real — so a client that
        // only knows the bool fails closed on a shadow packet — and an
        // N-1 body that sends only `simulated: true` reads as the
        // simulated company.
        let job = Job::new(
            "test-kind",
            Subject::new("asset", "sys-001"),
            "Test job",
            "emp-42",
            Priority::Standard,
            NaiveDate::from_ymd_opt(2026, 4, 16).unwrap(),
        )
        .with_partition(Partition::Shadow);
        let v = serde_json::to_value(&job).unwrap();
        assert_eq!(v["partition"], "shadow");
        assert_eq!(v["simulated"], true);
        let back: Job = serde_json::from_value(v).unwrap();
        assert_eq!(back, job);

        let mut legacy = serde_json::to_value(&job).unwrap();
        legacy.as_object_mut().unwrap().remove("partition");
        legacy["simulated"] = serde_json::Value::Bool(true);
        let back: Job = serde_json::from_value(legacy).unwrap();
        assert_eq!(back.partition, Partition::Simulated);
    }

    #[test]
    fn step_serde_round_trip() {
        let job_id = JobId::new();
        let step = Step::new(job_id, "generic", "QA passed", 3)
            .with_assignee("emp-10")
            .with_sign_offs_required(vec!["qa-lead".into()]);
        let json = serde_json::to_string(&step).unwrap();
        let back: Step = serde_json::from_str(&json).unwrap();
        assert_eq!(step, back);
    }

    #[test]
    fn subject_serializes_with_tag() {
        // Subject is uniformly (kind, id). Wire shape is
        // `{"subject_kind": "<kind>", "id": "..."}` regardless of
        // whether the kind is a platform kind or tenant-defined.
        let s = Subject::new("asset", "SN-001");
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains(r#""subject_kind":"asset""#));
        assert!(json.contains(r#""id":"SN-001""#));

        let s = Subject::new("asset", "A-1");
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains(r#""subject_kind":"asset""#));
        assert!(json.contains(r#""id":"A-1""#));
    }

    #[test]
    fn job_status_kebab_case() {
        for (s, wire) in [
            (JobStatus::Draft, r#""draft""#),
            (JobStatus::Open, r#""open""#),
            (JobStatus::Closed, r#""closed""#),
            (JobStatus::Cancelled, r#""cancelled""#),
        ] {
            let json = serde_json::to_string(&s).unwrap();
            assert_eq!(json, wire);
            let back: JobStatus = serde_json::from_str(&json).unwrap();
            assert_eq!(back, s);
        }
    }

    /// `blocked` and `pending-sign-off` were retired (backlog 3c3dc8f3):
    /// no Job and no `jobs.job.*` event ever held either. A retired word
    /// must be REFUSED where it arrives — a query parameter answers 400
    /// naming the four live statuses — never read as some other status,
    /// because a filter that silently widens or narrows is a wrong
    /// answer that looks like a result.
    #[test]
    fn a_retired_job_status_is_refused_not_read_as_another() {
        for retired in [r#""blocked""#, r#""pending-sign-off""#] {
            let parsed = serde_json::from_str::<JobStatus>(retired);
            assert!(parsed.is_err(), "{retired} still parses: {parsed:?}");
        }
    }

    #[test]
    fn priority_kebab_case() {
        let p = Priority::Emergency;
        let json = serde_json::to_string(&p).unwrap();
        assert_eq!(json, r#""emergency""#);
    }

    #[test]
    fn step_status_kebab_case() {
        let s = StepStatus::Active;
        let json = serde_json::to_string(&s).unwrap();
        assert_eq!(json, r#""active""#);
    }

    #[test]
    fn job_new_defaults() {
        let job = Job::new(
            "test-kind",
            Subject::new("account", "acc-001"),
            "Test job",
            "emp-1",
            Priority::Urgent,
            NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
        );
        assert_eq!(job.status, JobStatus::Draft);
        assert_eq!(job.metadata, serde_json::json!({}));
        assert!(job.tags.is_empty());
        assert!(job.due_on.is_none());
        assert!(job.closed_on.is_none());
    }

    #[test]
    fn step_new_defaults() {
        let step = Step::new(JobId::new(), "generic", "Do the thing", 0);
        assert_eq!(step.status, StepStatus::Pending);
        assert!(step.sign_offs_required.is_empty());
        assert!(step.sign_offs.is_empty());
        assert!(step.assignee_id.is_none());
        assert!(step.blocked_by.is_empty());
        assert_eq!(step.metadata, serde_json::json!({}));
    }

    #[test]
    fn step_field_filled_by_defaults_to_executor_when_absent() {
        // Every StepField written before `filled_by` existed — TOML
        // seeds, JSON registry rows, STEP_CREATED payloads in the
        // audit log — reads as Executor: required-at-done, today's
        // contract, unchanged.
        let f: StepField = serde_json::from_value(serde_json::json!({
            "name": "scheduled_at",
            "field_type": "date-time",
            "required": true,
        }))
        .unwrap();
        assert_eq!(f.filled_by, FilledBy::Executor);
        assert_eq!(FilledBy::default(), FilledBy::Executor);
    }

    #[test]
    fn step_field_filled_by_round_trips_kebab_case() {
        let f = StepField {
            name: "markdown".into(),
            field_type: "string".into(),
            required: true,
            filled_by: FilledBy::Filer,
            item_keys: Vec::new(),
            covers: None,
            binds: None,
            item_value_max_bytes: None,
            item_one_of: Vec::new(),
        };
        let json = serde_json::to_value(&f).unwrap();
        assert_eq!(json["filled_by"], serde_json::json!("filer"));
        let back: StepField = serde_json::from_value(json).unwrap();
        assert_eq!(back, f);

        // And the default serializes explicitly, so a registry row
        // says what it means rather than leaning on a reader's
        // default.
        let exec = StepField {
            filled_by: FilledBy::Executor,
            item_keys: Vec::new(),
            covers: None,
            binds: None,
            item_value_max_bytes: None,
            item_one_of: Vec::new(),
            ..f
        };
        let json = serde_json::to_value(&exec).unwrap();
        assert_eq!(json["filled_by"], serde_json::json!("executor"));
    }

    /// `binds` and `item_value_max_bytes` (design 26a89f11, exhibits)
    /// default to absent — every field authored before them reads as
    /// unbound and unbounded — and round-trip when a protocol states
    /// them, so a registry row carries the contract it was authored with.
    #[test]
    fn step_field_binds_and_value_bound_default_absent_and_round_trip() {
        let bare: StepField = serde_json::from_value(serde_json::json!({
            "name": "questions",
            "field_type": "array",
        }))
        .unwrap();
        assert_eq!(bare.binds, None);
        assert_eq!(bare.item_value_max_bytes, None);
        assert!(bare.item_one_of.is_empty());
        let json = serde_json::to_value(&bare).unwrap();
        assert!(json.get("binds").is_none() && json.get("item_value_max_bytes").is_none());
        assert!(json.get("item_one_of").is_none());

        let stated: StepField = serde_json::from_value(serde_json::json!({
            "name": "exhibits",
            "field_type": "array",
            "binds": "other",
            "item_value_max_bytes": 262144,
            "item_one_of": ["html", "file_ref"],
        }))
        .unwrap();
        assert_eq!(stated.binds.as_deref(), Some("other"));
        assert_eq!(stated.item_value_max_bytes, Some(262_144));
        assert_eq!(stated.item_one_of, vec!["html", "file_ref"]);
        let back: StepField =
            serde_json::from_value(serde_json::to_value(&stated).unwrap()).unwrap();
        assert_eq!(back, stated);
    }

    #[test]
    fn shape_hash_is_key_order_independent() {
        let a = serde_json::json!({"qty": 5, "sku": "X"});
        let b = serde_json::json!({"sku": "X", "qty": 5});
        assert_eq!(step_shape_hash("t", &a), step_shape_hash("t", &b));
    }

    #[test]
    fn shape_hash_changes_with_content() {
        let a = serde_json::json!({"qty": 5});
        let b = serde_json::json!({"qty": 6});
        assert_ne!(step_shape_hash("t", &a), step_shape_hash("t", &b));
        assert_ne!(step_shape_hash("t", &a), step_shape_hash("u", &a));
    }

    /// A KEY IS ENCODED, SO NO TWO SHAPES SHARE A CANONICAL FORM
    /// (security review of fd7090cc, 2026-09-24). Keys were written raw,
    /// `key:value,`, so a key carrying `:` and `,` could spell a
    /// neighbour: `{"zz":1,"zzz":2}` and `{"zz:1,zzz":2}` both became
    /// `{zz:1,zzz:2,}`, and a sign-off stamp over one shape verified on
    /// the other. infra/ops/ops-runner.sh recomputes this hash in jq and
    /// is pinned equal to it by ops_runner_approval_sh.rs.
    #[test]
    fn shape_hash_keys_cannot_collide_with_a_neighbouring_key() {
        let a = serde_json::json!({"zz": 1, "zzz": 2});
        let b = serde_json::json!({"zz:1,zzz": 2});
        assert_ne!(step_shape_hash("t", &a), step_shape_hash("t", &b));
    }

    #[test]
    fn cross_job_blocked_by() {
        let job_a = JobId::new();
        let job_b = JobId::new();
        let step_a = Step::new(job_a, "generic", "Step A", 0);
        let step_b = Step::new(job_b, "generic", "Step B", 0).with_blocked_by(vec![step_a.id]);
        assert_eq!(step_b.blocked_by.len(), 1);
        assert_eq!(step_b.blocked_by[0], step_a.id);
    }
}
