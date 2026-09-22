//! `boss triage` / `boss fold` / `boss hold` / `boss release` — the
//! step writes an operator kept hand-building, with the body READ off
//! the step's own declared fields instead of typed from memory.
//!
//! WHY. Retro 27fad542 counted ten hand-built `boss-api PUT
//! /api/jobs/{id}/steps/{sid}` calls in two days (window
//! 2026-09-16T23:55Z to 09-18T14:00Z): three triage dispositions on
//! backlog-items, three design-doc fold steps, one decision packet's
//! build step, and three or four review-step holds and releases on
//! parked cars (backlog 0d2e1655). Every one carried a body typed from
//! memory of the step's field names — `disposition`, `evidence`,
//! `fold_change`, `hold` — the retro's own class B, a value typed
//! instead of read. A field name typed wrong does not error: the API
//! merges the body over the step and stores an unknown key as an
//! annotation, while the required one stays missing until
//! required-at-done refuses it — or, on a step with nothing required,
//! never. `boss job file` and `boss gate` retired the same class of
//! curl for filing; these retire it for completing.
//!
//! THE BODY IS READ, NOT TYPED. A materialised step carries the
//! `fields` its Workflow row declared, at the version the packet is
//! pinned to — the very list `update_step` validates a completion
//! against (`validate_authored_fields(&step.fields, ..)`). Each verb
//! reads that list off the packet and refuses what the row does not
//! declare: a disposition outside the enum is refused NAMING the enum;
//! `--evidence` lands in whichever text field the row declares
//! (`evidence` on a backlog-item, `finding` on user-feedback) and is
//! refused when the row declares neither; a fold on a doc whose review
//! is not done is refused naming the anchors still open. The one key
//! no row declares is `hold`: it is a MARKER, read by
//! `boss_jobs::stranded::hold_reason` on the car's review step, and
//! written here in exactly that shape — a non-blank string to hold,
//! the key removed to release.
//!
//! CONFIRMATION OVER STATUS CODES, as `boss job patch` does. Every
//! write reads the packet back and fails unless the step holds what
//! was sent, then prints where the packet stands NOW — the next open
//! step and who holds it — because that is the question the operator
//! asked next, by hand, every time.
//!
//! Signed as the actor running the verb (`identity`), never the
//! conductor (5083d6f5): an unnamed write is refused before it goes.

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Map, Value, json};

use crate::identity;

/// Top-level registration — see `merged::Cmd` for why the variants
/// live in their verb's module (84f9fbc0).
#[derive(clap::Subcommand)]
pub enum Cmd {
    /// Complete a packet's ready `triage` step, in the shape its Workflow row declares.
    ///
    /// The disposition must be one of the values the step's
    /// `disposition` field enumerates (the refusal names them), the
    /// evidence lands in the text field the row declares, and
    /// `duplicate` records `--of` as `duplicate_of`. Reads the packet
    /// back and prints the step it stands at now.
    Triage {
        /// The backlog-item or user-feedback: 8+ characters of its id, or the full uuid.
        item: String,
        /// The route: one of the values the step's `disposition` field declares.
        disposition: String,
        /// What was measured. Written to the text field the row declares
        /// (`evidence` on a backlog-item, `finding` on user-feedback).
        /// SINGLE-quote it: inside double quotes a backticked word is
        /// run by the shell and lands as a hole (backlog 2376b89e).
        #[arg(long, required_unless_present = "evidence_file")]
        evidence: Option<String>,
        /// The evidence, read from this file — no shell between the
        /// bytes and the record. Exclusive with --evidence.
        #[arg(long, conflicts_with = "evidence")]
        evidence_file: Option<std::path::PathBuf>,
        /// With `duplicate`: the packet this one duplicates (8+ characters
        /// of its id, or the full uuid). Recorded as `duplicate_of`.
        #[arg(long)]
        of: Option<String>,
    },
    /// Complete a design-doc's ready `fold` step with what current truth gained.
    ///
    /// Refuses while the review is not done, naming the anchors still
    /// open. Prints the terminal the doc reached.
    Fold {
        /// The design-doc: 8+ characters of its id, or the full uuid.
        design: String,
        /// What current truth gains from this doc, and where — the row's
        /// `fold_change`. SINGLE-quote it: inside double quotes a
        /// backticked word is run by the shell and lands as a hole
        /// (backlog 2376b89e).
        #[arg(long, required_unless_present = "change_file")]
        change: Option<String>,
        /// The change, read from this file — no shell between the bytes
        /// and the record. Exclusive with --change.
        #[arg(long, conflicts_with = "change")]
        change_file: Option<std::path::PathBuf>,
        /// WHERE the settled material landed — the file and section, or
        /// the car that carried it. Write `none` when the fold changed
        /// nothing. Required only when the row declares `folded_into`
        /// (design-doc from the version that added it; an in-flight
        /// packet pinned to an earlier one does not take it).
        /// SINGLE-quote it, like --change.
        #[arg(long)]
        folded_into: Option<String>,
    },
    /// Hold a parked car at the dock: it stays gated green and does not board.
    ///
    /// The hold is `metadata.hold` on the car's review step — the marker
    /// the conductor, the loading-dock row and `boss orient` all read.
    Hold {
        /// The car: its branch, or 8+ characters of its id.
        car: String,
        /// Why it must not ride yet. Recorded verbatim; the yard shows it.
        #[arg(long)]
        reason: String,
    },
    /// Release a held car: the hold comes off its review step and it boards at the next tick.
    Release {
        /// The car: its branch, or 8+ characters of its id.
        car: String,
    },
    /// Step verbs that belong to no one protocol — today, the generic completion.
    Step {
        #[command(subcommand)]
        action: StepAction,
    },
}

/// The generic half of this module. A new step verb lands INSIDE this
/// enum rather than as another top-level variant: a group's action enum
/// touches no shared line, so two in-flight cars adding one merge clean
/// (the rule `main::Commands` states, 84f9fbc0).
#[derive(clap::Subcommand)]
pub enum StepAction {
    /// Complete any step, in the shape its Workflow row declares.
    ///
    /// The five specific verbs above cover five step kinds; this one
    /// covers the rest, and it is what the page march's ~94 completions
    /// go through. It reads the step's declared fields off the packet,
    /// REFUSES a name the row does not declare (the API would store it
    /// as an annotation and answer success), merges the writes over the
    /// step's own metadata rather than replacing it, judges the result
    /// by the registry's own validator before the round trip, and reads
    /// the packet back — a 204 is not evidence.
    Complete {
        /// The packet: 8+ characters of its id, or the full uuid.
        packet: String,
        /// The step's slug, as the Workflow row titles it (`measure`, `file`, `test`).
        #[arg(long)]
        step: String,
        /// One declared field: `--field name=value`, repeatable. Typed
        /// by what the row declares — a `number` is written as a
        /// number, an enum value is checked against its set, an
        /// `array`/`object` value is parsed as JSON. SINGLE-quote
        /// prose: inside double quotes a backticked word is run by the
        /// shell and lands as a hole (backlog 2376b89e).
        #[arg(long = "field", value_name = "NAME=VALUE")]
        field: Vec<String>,
        /// A field whose value is read from a file: `--field-file
        /// name=PATH`, repeatable. The door for a long body — the
        /// march's `controls_md`, `needs_md`, `gaps_md` are whole
        /// documents — with no shell between the bytes and the record.
        #[arg(long = "field-file", value_name = "NAME=PATH")]
        field_file: Vec<String>,
    },
    /// Hand a claimed step back to its station: `ready`, unassigned, no run named.
    ///
    /// The door for a step whose executor never came back and which no
    /// rule can free — a step claimed before the run edge existed
    /// names nobody, so `an-abandoned-step-is-reclaimed-when-its-run-died`
    /// leaves it alone permanently and correctly (backlog e871febf).
    /// Refuses a step nobody holds, records `--why` on the step, and
    /// reads the packet back.
    ///
    /// THE OPERATOR IS THE GUARD. The clock rule releases only after a
    /// run is recorded dead and a further bound has passed, because a
    /// reclaim is the one move that can put two executors on one step.
    /// This verb releases on your word and checks nothing about who
    /// might still be building — read the run first.
    Release {
        /// The packet: 8+ characters of its id, or the full uuid.
        packet: String,
        /// The step's slug, as the Workflow row titles it (`build`, `measure`).
        #[arg(long)]
        step: String,
        /// Why it is being taken back. Recorded verbatim under
        /// `released`, and it is the only thing that tells the next
        /// reader this step came free by a hand. SINGLE-quote it:
        /// inside double quotes a backticked word is run by the shell
        /// and lands as a hole (backlog 2376b89e).
        #[arg(long)]
        why: String,
    },
}

pub async fn dispatch(cmd: Cmd) -> Result<()> {
    let wire = Wire::live()?;
    match cmd {
        Cmd::Triage {
            item,
            disposition,
            evidence,
            evidence_file,
            of,
        } => {
            let evidence = crate::prose::text_or_file(
                "--evidence",
                "--evidence-file",
                evidence,
                evidence_file.as_deref(),
            )?;
            triage(&wire, &item, &disposition, &evidence, of.as_deref()).await
        }
        Cmd::Fold {
            design,
            change,
            change_file,
            folded_into,
        } => {
            let change = crate::prose::text_or_file(
                "--change",
                "--change-file",
                change,
                change_file.as_deref(),
            )?;
            fold(&wire, &design, &change, folded_into.as_deref()).await
        }
        Cmd::Hold { car, reason } => hold(&wire, &car, Some(&reason)).await,
        Cmd::Release { car } => hold(&wire, &car, None).await,
        Cmd::Step { action } => match action {
            StepAction::Complete {
                packet,
                step,
                field,
                field_file,
            } => {
                let given = given_values(&field, &field_file)?;
                complete(&wire, &packet, &step, &given).await
            }
            StepAction::Release { packet, step, why } => {
                release_step(&wire, &packet, &step, &why).await
            }
        },
    }
}

// ----------------------------------------------------------------------
// The pure core: which step, which fields, what body, what standing.
// ----------------------------------------------------------------------

/// A field as the Workflow row declared it, read off the materialised
/// step (`Step.fields`, `boss_core::job::StepField`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Field {
    pub name: String,
    pub field_type: String,
    pub required: bool,
}

/// The step's declared completion contract, in the row's own order.
pub(crate) fn declared_fields(step: &Value) -> Vec<Field> {
    step.get("fields")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|f| {
            Some(Field {
                name: f.get("name")?.as_str()?.to_string(),
                field_type: f
                    .get("field_type")
                    .and_then(Value::as_str)
                    .unwrap_or("string")
                    .to_string(),
                required: f.get("required").and_then(Value::as_bool).unwrap_or(false),
            })
        })
        .collect()
}

/// The fields a step's KIND declares, on top of its row's.
///
/// TWO VALIDATORS HAD TO AGREE AND DID NOT (backlog b1e87213). The API
/// judges a completion against the StepType's schema AND the row's;
/// this verb judged it against the row alone. So `checklist` — whose
/// kind requires an `items` array and whose three uses in the platform
/// bundle declare it in no row at all — could not be completed through
/// this door in either direction: the API refused the body without
/// `items`, and the verb refused to send `items` because the row did
/// not name it.
///
/// Measured cost, 2026-09-21: publish-to-github packet 7d5c9051 sat at
/// `measure` from 2026-09-20, the daily publish rule guards on `NOT
/// open_publish_exists`, and so one uncompletable step suppressed the
/// public mirror's publish entirely — 455 commits behind by the time
/// anyone reached for the door. It read as nobody having worked the
/// packet. It was a door that could not be opened.
///
/// The row WINS on a name collision: a row may narrow a kind's field
/// (a tighter enum, a required that the kind leaves optional), and the
/// row is the more specific contract.
pub(crate) fn with_kind_fields(row: Vec<Field>, kind_fields: Vec<Field>) -> Vec<Field> {
    let mut out = row;
    for f in kind_fields {
        if !out.iter().any(|d| d.name == f.name) {
            out.push(f);
        }
    }
    out
}

/// The StepType registry's fields for one kind, off `GET
/// /api/jobs/step-types`. An unknown kind contributes nothing — the
/// API is still the judge, and a verb that invented a contract the
/// registry does not hold would be the defect one layer over.
pub(crate) fn kind_fields(step_types: &Value, kind: &str) -> Vec<Field> {
    let rows = step_types
        .get("data")
        .and_then(Value::as_array)
        .or_else(|| step_types.as_array());
    rows.into_iter()
        .flatten()
        .find(|t| t.get("kind").and_then(Value::as_str) == Some(kind))
        .map(declared_fields)
        .unwrap_or_default()
}

/// The values an enum field admits — `a|b|c` is the registry's enum
/// spelling (`step_registry::validate_field_type`); anything without a
/// pipe is a scalar type, not an enum.
pub(crate) fn enum_values(field_type: &str) -> Option<Vec<&str>> {
    field_type
        .contains('|')
        .then(|| field_type.split('|').map(str::trim).collect())
}

fn short(packet: &Value) -> String {
    crate::train::id8(crate::envelope::job_id(packet).unwrap_or("?"))
}

fn kind_of(packet: &Value) -> &str {
    packet.get("kind").and_then(Value::as_str).unwrap_or("?")
}

fn status_of(step: &Value) -> &str {
    step.get("status").and_then(Value::as_str).unwrap_or("?")
}

fn is_open(step: &Value) -> bool {
    matches!(status_of(step), "ready" | "active")
}

/// Who holds a step: its assignee, else the role it waits on, else
/// nobody — the same three readings `boss job get` prints per row.
pub(crate) fn holder(step: &Value) -> String {
    let line = crate::envelope::step_line(step);
    match (line.assignee, line.authority_role) {
        (Some(who), _) => who,
        (None, Some(role)) => format!("role {role} (unassigned)"),
        (None, None) => "nobody".to_string(),
    }
}

/// Where the packet stands: every open step with its holder, or the
/// terminal it closed on. Prose a person reads after a completion.
pub(crate) fn standing(packet: &Value) -> String {
    let open: Vec<String> = crate::envelope::steps(packet)
        .into_iter()
        .filter(|s| is_open(s))
        .map(|s| {
            format!(
                "`{}` ({}) held by {}",
                s.get("spec_slug").and_then(Value::as_str).unwrap_or("?"),
                status_of(s),
                holder(s)
            )
        })
        .collect();
    if !open.is_empty() {
        return format!("now at {}", open.join(", "));
    }
    match packet.get("status").and_then(Value::as_str) {
        Some("closed" | "cancelled") => format!(
            "closed — outcome {}",
            packet
                .pointer("/metadata/outcome")
                .and_then(Value::as_str)
                .unwrap_or("unrecorded")
        ),
        _ => "no step is open".to_string(),
    }
}

/// The `slug` step, ready or active — the one a completion may write
/// to. Refused otherwise, naming where the packet stands instead, so
/// the operator is not sent to `boss job get` to learn it.
pub(crate) fn open_step<'a>(packet: &'a Value, slug: &str) -> Result<&'a Value, String> {
    let step = crate::envelope::steps(packet)
        .into_iter()
        .find(|s| s.get("spec_slug").and_then(Value::as_str) == Some(slug))
        .ok_or_else(|| {
            format!(
                "packet {} ({}) has no `{slug}` step — {}",
                short(packet),
                kind_of(packet),
                standing(packet)
            )
        })?;
    if is_open(step) {
        Ok(step)
    } else {
        Err(format!(
            "packet {}'s `{slug}` is {} — {}",
            short(packet),
            status_of(step),
            standing(packet)
        ))
    }
}

/// The completion body: `writes` laid over the step's own metadata.
/// PATCH-on-PUT replaces `metadata` wholesale, and `authority_role`,
/// `audience` and `procedure` live there — so the existing keys ride
/// through and only the declared fields change.
pub(crate) fn completion(step: &Value, writes: &Map<String, Value>) -> Value {
    let mut md = step
        .get("metadata")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    md.extend(writes.iter().map(|(k, v)| (k.clone(), v.clone())));
    json!({ "status": "completed", "metadata": Value::Object(md) })
}

/// The text fields a triage row may declare for its measurement, in
/// preference order: `evidence` (backlog-item) and `finding`
/// (user-feedback). The row decides which one is written.
const EVIDENCE_FIELDS: [&str; 2] = ["evidence", "finding"];

/// What `boss triage` writes, decided against the step's declared
/// fields — pure, so every refusal is pinned by name.
pub(crate) fn triage_writes(
    step: &Value,
    disposition: &str,
    evidence: &str,
    duplicate_of: Option<&str>,
) -> Result<Map<String, Value>, String> {
    let fields = declared_fields(step);
    let names = || {
        fields
            .iter()
            .map(|f| f.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    };
    let route = fields
        .iter()
        .find(|f| f.name == "disposition")
        .ok_or_else(|| {
            format!(
                "this triage step declares no `disposition` field (it declares: {}) — \
                 nothing here can complete it",
                names()
            )
        })?;
    let allowed = enum_values(&route.field_type).ok_or_else(|| {
        format!(
            "the row declares `disposition` as `{}`, not an enum — refusing to guess a route",
            route.field_type
        )
    })?;
    if !allowed.contains(&disposition) {
        return Err(format!(
            "`{disposition}` is not a disposition this row declares — one of [{}]",
            allowed.join(", ")
        ));
    }
    let text = EVIDENCE_FIELDS
        .iter()
        .find(|name| fields.iter().any(|f| f.name == **name))
        .ok_or_else(|| {
            format!(
                "this triage step declares no text field for the measurement (looked for \
                 {}; it declares: {}) — refusing to invent one",
                EVIDENCE_FIELDS.join(" or "),
                names()
            )
        })?;
    if evidence.trim().is_empty() {
        return Err(format!(
            "--evidence is blank — `{text}` is what the next reader measures against, and a \
             disposition with nothing behind it is the class B write this verb exists to stop"
        ));
    }
    let mut writes = Map::new();
    writes.insert("disposition".into(), json!(disposition));
    writes.insert((*text).into(), json!(evidence.trim()));
    match (disposition == "duplicate", duplicate_of) {
        (true, Some(of)) => {
            writes.insert("duplicate_of".into(), json!(of));
        }
        (true, None) => {
            return Err(
                "`duplicate` needs --of <packet>: the original this one duplicates is \
                        recorded as `duplicate_of`, and a duplicate that names nothing sends \
                        the next reader hunting for it"
                    .to_string(),
            );
        }
        (false, Some(_)) => {
            return Err(format!(
                "--of only accompanies `duplicate` — `{disposition}` records no original"
            ));
        }
        (false, None) => {}
    }
    Ok(writes)
}

/// The anchors the design's review still owes an answer: every
/// `questions[].anchor` with no `resolutions[]` entry of the same
/// anchor — the `covers = "questions"` contract, read the way the row
/// states it.
pub(crate) fn open_anchors(review: &Value) -> Vec<String> {
    let anchors = |key: &str| -> Vec<String> {
        review
            .pointer(&format!("/metadata/{key}"))
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|q| q.get("anchor").and_then(Value::as_str))
            .map(str::to_string)
            .collect()
    };
    let resolved = anchors("resolutions");
    anchors("questions")
        .into_iter()
        .filter(|a| !resolved.contains(a))
        .collect()
}

/// The `fold` step a design may be folded at. A fold is `ready_when =
/// steps.review.done`, so a pending fold means the review is not done —
/// and the refusal names the anchors still open rather than the fold's
/// status, because the anchors are what the operator goes to answer.
pub(crate) fn foldable(packet: &Value) -> Result<&Value, String> {
    match open_step(packet, "fold") {
        Ok(step) => Ok(step),
        Err(refusal) => {
            let review = crate::envelope::steps(packet)
                .into_iter()
                .find(|s| s.get("spec_slug").and_then(Value::as_str) == Some("review"));
            match review {
                Some(r) if !matches!(status_of(r), "completed" | "skipped") => {
                    let open = open_anchors(r);
                    Err(format!(
                        "design {}'s review is {} — {}",
                        short(packet),
                        status_of(r),
                        if open.is_empty() {
                            "its questions are all answered but the review step is not \
                             completed; complete it at /it/design first"
                                .to_string()
                        } else {
                            format!(
                                "{} question(s) still open: {}. Decide them at /it/design first",
                                open.len(),
                                open.join(", ")
                            )
                        }
                    ))
                }
                _ => Err(refusal),
            }
        }
    }
}

/// What `boss fold` writes: `fold_change`, and `folded_into` when the
/// row declares it.
///
/// THE ROW DECIDES, NOT THE FLAG (backlog aaf85ca2). `folded_into`
/// arrives in a later design-doc version, and an in-flight packet stays
/// pinned to the version it was admitted under — so this one verb must
/// complete a fold on a row that declares the field and on one that
/// does not. Writing a field the row never declared would be refused at
/// completion; refusing to fold an older packet would strand it.
pub(crate) fn fold_writes(
    step: &Value,
    change: &str,
    folded_into: Option<&str>,
) -> Result<Map<String, Value>, String> {
    let fields = declared_fields(step);
    if !fields.iter().any(|f| f.name == "fold_change") {
        return Err(format!(
            "this fold step declares no `fold_change` field (it declares: {}) — nothing here \
             can complete it",
            fields
                .iter()
                .map(|f| f.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if change.trim().is_empty() {
        return Err(
            "--change is blank — 'nothing' is a legitimate fold and still has to be \
                    written down with its reason; that is the whole point of the step"
                .to_string(),
        );
    }
    let mut writes = Map::new();
    writes.insert("fold_change".into(), json!(change.trim()));
    if fields.iter().any(|f| f.name == "folded_into") {
        let landed = folded_into.unwrap_or("").trim();
        if landed.is_empty() {
            return Err(
                "--folded-into is blank, and this fold step declares the field — write \
                 where the settled material landed (the file and section, or the car \
                 that carried it), or `none` when the fold changed nothing. It is the \
                 only place that tie is recorded: the terminal has no field for it, and \
                 filing time cannot know it"
                    .to_string(),
            );
        }
        writes.insert("folded_into".into(), json!(landed));
    }
    Ok(writes)
}

/// The review step a hold goes on — open, because a hold on a car past
/// review is a note on a terminal step (the API refuses it, 409) and a
/// hold on a car not yet gated has nothing to brake.
pub(crate) fn holdable(car: &Value) -> Result<&Value, String> {
    let review =
        boss_jobs::car::find_step(car, boss_jobs::car::REVIEW_SLUG, boss_jobs::car::REVIEW)
            .ok_or_else(|| {
                format!(
                    "car {} ({}) has no review step — a hold brakes a car at the dock, and this \
                 packet is not one",
                    short(car),
                    kind_of(car)
                )
            })?;
    if is_open(review) {
        Ok(review)
    } else {
        Err(format!(
            "car {}'s review is {} — a hold brakes a car standing at the dock (review open); {}",
            short(car),
            status_of(review),
            standing(car)
        ))
    }
}

/// The review step's metadata with the hold on — the marker in the one
/// shape `stranded::hold_reason` reads (a non-blank string). Every
/// other key rides through: PATCH-on-PUT replaces metadata wholesale.
pub(crate) fn hold_metadata(review: &Value, reason: &str) -> Value {
    let mut md = review
        .get("metadata")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    md.insert("hold".into(), json!(reason.trim()));
    Value::Object(md)
}

/// The review step's metadata with the hold OFF — the key removed, not
/// nulled or falsed. `hold_reason` reads `false`/`""` as released too,
/// but a released car that still carries the key reads as "held then
/// released" to every eye that is not that function.
pub(crate) fn release_metadata(review: &Value) -> Value {
    let mut md = review
        .get("metadata")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    md.remove("hold");
    Value::Object(md)
}

// ----------------------------------------------------------------------
// The wire: one signed client, the reads and the read-backs.
// ----------------------------------------------------------------------

/// The jobs API as these verbs speak to it: one base, one caller. The
/// caller is a VALUE here, not read from the environment inside every
/// call, so a test can hand in a named actor and drive the whole
/// verb against a stub socket without touching the process env.
pub(crate) struct Wire {
    http: reqwest::Client,
    base: String,
    caller: Option<identity::Caller>,
}

impl Wire {
    /// The live one: the system of record `BOSS_JOBS_URL` names and the
    /// actor `BOSS_ACTOR` (or the actor file) names.
    pub(crate) fn live() -> Result<Self> {
        Ok(Self::at(
            crate::gate::resolve_jobs_base(None)?,
            identity::caller(),
        ))
    }

    pub(crate) fn at(base: String, caller: Option<identity::Caller>) -> Self {
        Self {
            http: reqwest::Client::new(),
            base,
            caller,
        }
    }

    /// One signed call. `pub(crate)` since 13d1fff3: the cadence
    /// write verbs speak through this same wire rather than a copy.
    pub(crate) async fn call(
        &self,
        method: reqwest::Method,
        path: &str,
        payload: Option<Value>,
    ) -> Result<Option<Value>> {
        let signature = identity::signature_for(&method, path, self.caller.clone());
        crate::gate::api_at_signed(&self.http, &self.base, method, path, payload, signature).await
    }

    /// The StepType registry, as the API's own validator reads it.
    /// Read fresh rather than compiled in: a kind's fields are
    /// registry data and a second copy here would be the drift 9a is
    /// written against.
    async fn step_types(&self) -> Result<Value> {
        self.call(reqwest::Method::GET, "/api/jobs/step-types", None)
            .await?
            .context("the step-type registry read back empty")
    }

    async fn packet(&self, id: &str) -> Result<Value> {
        self.call(reqwest::Method::GET, &format!("/api/jobs/{id}"), None)
            .await?
            .with_context(|| format!("packet {id} read back empty"))
    }

    /// Every open packet of `kind` (or of any kind), paged on `total`
    /// so a packet opened days ago is not left off page one
    /// (a-limit-is-not-a-filter).
    async fn open_rows(&self, kind: Option<&str>) -> Result<Vec<Value>> {
        let kind_q = kind.map(|k| format!("kind={k}&")).unwrap_or_default();
        crate::train::list_all_pages(|offset| {
            let path = format!(
                "/api/jobs?{kind_q}status=open&limit={}&offset={offset}",
                crate::train::PAGE_LIMIT
            );
            async move { self.call(reqwest::Method::GET, &path, None).await }
        })
        .await
    }

    /// A packet reference — the full uuid, or 8+ characters of it —
    /// resolved among the OPEN packets of `kind`, then read fresh by
    /// id. Open only: every step these verbs write to is open, and the
    /// `--of` original a duplicate names is almost always open too — a
    /// closed one takes its full uuid, which the refusal says.
    async fn resolve(&self, given: &str, kind: Option<&str>) -> Result<Value> {
        let id = if crate::job::looks_like_uuid(given) {
            given.to_string()
        } else {
            let rows = self.open_rows(kind).await?;
            crate::job::resolve(&rows, given)
                .map_err(|e| anyhow!("{e} (open packets only — a closed one takes its full uuid)"))?
                .get("id")
                .and_then(Value::as_str)
                .context("matched a packet with no id")?
                .to_string()
        };
        self.packet(&id).await
    }

    /// The one car for `given` — its branch or 8+ characters of its id
    /// — resolved the way `boss prove` resolves one (`prove::find_car`,
    /// live cars only), over every open car.
    async fn car(&self, given: &str) -> Result<Value> {
        let cars = self.open_rows(Some("ship-a-change")).await?;
        let car = crate::prove::find_car(&cars, given, crate::prove::Eligible::Live)?;
        let id = crate::envelope::job_id(car).context("matched a car with no id")?;
        self.packet(id).await
    }

    /// Who this wire signs as, for a record that names the hand rather
    /// than only the write. `None` is possible in shape only: an
    /// unnamed write is refused before it goes (`identity`).
    pub(crate) fn caller_id(&self) -> Option<&str> {
        self.caller.as_ref().map(|c| c.id.as_str())
    }

    /// The step's MERGE door: keys land one at a time and an explicit
    /// null DELETES one. The only door that can clear `agent_run`,
    /// which the PUT below carries forward on omission (b91a2103).
    async fn patch_step_metadata(&self, job_id: &str, step_id: &str, body: Value) -> Result<()> {
        self.call(
            reqwest::Method::PATCH,
            &format!("/api/jobs/{job_id}/steps/{step_id}/metadata"),
            Some(body),
        )
        .await
        .map(|_| ())
    }

    async fn put_step(&self, job_id: &str, step_id: &str, body: Value) -> Result<()> {
        self.call(
            reqwest::Method::PUT,
            &format!("/api/jobs/{job_id}/steps/{step_id}"),
            Some(body),
        )
        .await
        .map(|_| ())
    }
}

fn step_id(step: &Value) -> Result<&str> {
    step.get("id")
        .and_then(Value::as_str)
        .context("the step carries no id")
}

fn title_of(packet: &Value) -> &str {
    crate::envelope::job_title(packet).unwrap_or("?")
}

/// The step as the packet holds it after the write, by id.
fn step_after<'a>(packet: &'a Value, id: &str) -> Result<&'a Value> {
    crate::envelope::steps(packet)
        .into_iter()
        .find(|s| s.get("id").and_then(Value::as_str) == Some(id))
        .with_context(|| format!("the packet read back without step {id}"))
}

/// A 204 is a claim; the read-back is the fact. The step must be
/// completed and hold every key that was sent, or this fails naming
/// what it holds instead (`job::confirm_patch`'s report).
fn confirm_completed(step: &Value, writes: &Map<String, Value>) -> Result<()> {
    let now = step.get("metadata").cloned().unwrap_or_else(|| json!({}));
    let (report, all_took) = crate::job::confirm_patch(&now, &Value::Object(writes.clone()));
    if status_of(step) != "completed" {
        bail!(
            "the API answered the PUT but the step reads back as {} — not completed\n{report}",
            status_of(step)
        );
    }
    if !all_took {
        bail!(
            "the API answered the PUT but the step does not hold what was sent — a silent \
             no-op\n{report}"
        );
    }
    Ok(())
}

/// The warning a self-closing alarm earns when a human triages it.
///
/// WHY (backlog 2228dea3, measured 2026-09-22). An estate alarm closes
/// ITSELF: `estate.recover` watches the comparison series and, when the
/// finding stops appearing, completes the alarm's TRIAGE step with
/// disposition `stale`. Two `disk_tight:w-1` alarms closed that way on
/// 2026-09-18.
///
/// A third did not, and the reason was a race nobody could see:
///
/// ```text
///   14114f92  triage completed by automation:rule:estate-recover-on-comparison -> stale
///   e1fea3b2  triage completed by agent-claude                                 -> build
/// ```
///
/// Same finding, same scope, same host. Triaging it to `build` took the
/// step the recovery acts on, so when the condition cleared — 0 of 20
/// consecutive comparisons still carrying it — the recovery had nothing
/// to complete, the withdrawal terminals were already skipped, and an
/// urgent alarm sat open on the queue with its condition long gone.
///
/// NOTHING SAID SO, BEFORE OR AFTER, and that is the whole defect. An
/// estate alarm is a `backlog-item`: it sits in the backlog station,
/// `boss orient` lists it among the rest, and it has a `triage` step
/// like everything else. Triaging it looks exactly like ordinary queue
/// work, and doing it silently disables a mechanism that would have
/// closed the packet for free.
///
/// THE PREDICATE IS THE HANDLER'S OWN. `estate_recover::finding_of`
/// reads `metadata.estate_finding` and matches on it; so does this. Not
/// a guess about titles or kinds — the same key, so the warning cannot
/// disagree with the thing it is warning about (§9a).
///
/// A WARNING, NOT A REFUSAL. An operator may genuinely want to route an
/// alarm — one that will not clear on its own needs a human disposition
/// — and refusing that would leave no way to act on it at all.
pub(crate) fn self_closing_warning(packet: &Value, disposition: &str) -> Option<String> {
    let finding = packet
        .get("metadata")
        .and_then(|m| m.get("estate_finding"))
        .and_then(Value::as_str)?;
    if disposition == "stale" {
        // The disposition the recovery itself uses. Taking the step to
        // reach the same terminal is not taking anything over.
        return None;
    }
    Some(format!(
        "boss triage: WARNING — this packet is a self-closing estate alarm          (`{finding}`). `estate.recover` closes one by completing THIS step with          disposition `stale` once the finding stops appearing in the comparison series.          Triaging it `{disposition}` takes that over: the withdrawal terminals skip, and          if the condition clears later the recovery will have nothing to complete and the          alarm stays open with its cause long gone (measured on e1fea3b2, backlog          2228dea3). If the finding is live and needs a person, this is right; if you are          routing it because it appeared in the queue, let it close itself."
    ))
}

pub(crate) async fn triage(
    wire: &Wire,
    item: &str,
    disposition: &str,
    evidence: &str,
    of: Option<&str>,
) -> Result<()> {
    let packet = wire.resolve(item, None).await?;
    let step = open_step(&packet, "triage").map_err(|e| anyhow!("{e}"))?;
    // The original a duplicate names is RESOLVED, not copied: the
    // record carries the full id of a packet that exists, read back,
    // never the eight characters that were typed.
    let original = match of {
        Some(given) => Some(wire.resolve(given, None).await.with_context(|| {
            format!("--of {given}: the original a duplicate names must be a packet")
        })?),
        None => None,
    };
    let original_id = original
        .as_ref()
        .map(|o| crate::envelope::job_id(o).context("the original has no id"))
        .transpose()?;
    if let Some(w) = self_closing_warning(&packet, disposition) {
        eprintln!("{w}");
    }
    let writes = triage_writes(step, disposition, evidence, original_id)
        .map_err(|e| anyhow!("{}: {e}", short(&packet)))?;
    let jid = crate::envelope::job_id(&packet).context("the packet has no id")?;
    let sid = step_id(step)?;
    wire.put_step(jid, sid, completion(step, &writes)).await?;

    let after = wire.packet(jid).await?;
    confirm_completed(step_after(&after, sid)?, &writes)?;
    println!(
        "boss triage: {} \"{}\" — triage completed: {disposition}{}\n  {}",
        short(&packet),
        title_of(&packet),
        original
            .as_ref()
            .map(|o| format!(" of {} \"{}\"", short(o), title_of(o)))
            .unwrap_or_default(),
        standing(&after)
    );
    Ok(())
}

pub(crate) async fn fold(
    wire: &Wire,
    design: &str,
    change: &str,
    folded_into: Option<&str>,
) -> Result<()> {
    let packet = wire.resolve(design, Some("design-doc")).await?;
    let step = foldable(&packet).map_err(|e| anyhow!("{e}"))?;
    let writes =
        fold_writes(step, change, folded_into).map_err(|e| anyhow!("{}: {e}", short(&packet)))?;
    let jid = crate::envelope::job_id(&packet).context("the packet has no id")?;
    let sid = step_id(step)?;
    wire.put_step(jid, sid, completion(step, &writes)).await?;

    let after = wire.packet(jid).await?;
    confirm_completed(step_after(&after, sid)?, &writes)?;
    println!(
        "boss fold: {} \"{}\" — fold completed; {}",
        short(&packet),
        title_of(&packet),
        standing(&after)
    );
    Ok(())
}

/// `Some(reason)` holds, `None` releases — one path, because both are
/// the same PUT of the review step's metadata with one key present or
/// absent, and the read-back checks the key the same way the readers
/// do (`stranded::hold_reason`).
pub(crate) async fn hold(wire: &Wire, car: &str, reason: Option<&str>) -> Result<()> {
    if let Some(r) = reason
        && r.trim().is_empty()
    {
        bail!(
            "--reason is blank — the yard shows the reason beside the held car, and a bare \
             hold reads 'no reason recorded' to every operator after you"
        );
    }
    let packet = wire.car(car).await?;
    let review = holdable(&packet).map_err(|e| anyhow!("{e}"))?;
    let branch = packet
        .pointer("/metadata/branch")
        .and_then(Value::as_str)
        .unwrap_or("?");
    let already = boss_jobs::stranded::hold_reason(review.get("metadata").unwrap_or(&Value::Null));
    let metadata = match reason {
        Some(r) => hold_metadata(review, r),
        None => {
            if already.is_none() {
                println!(
                    "boss release: {} {branch} \"{}\" carries no hold — nothing to release",
                    short(&packet),
                    title_of(&packet)
                );
                return Ok(());
            }
            release_metadata(review)
        }
    };
    let jid = crate::envelope::job_id(&packet).context("the car has no id")?;
    let sid = step_id(review)?;
    wire.put_step(jid, sid, json!({ "metadata": metadata }))
        .await?;

    let after = wire.packet(jid).await?;
    let now = boss_jobs::stranded::hold_reason(
        step_after(&after, sid)?
            .get("metadata")
            .unwrap_or(&Value::Null),
    );
    match (reason, now) {
        (Some(r), Some(held)) if held == r.trim() => println!(
            "boss hold: {} {branch} \"{}\" — held: {held}\n  it stays at the dock and does not \
             board until `boss release {branch}`",
            short(&packet),
            title_of(&packet)
        ),
        (Some(_), held) => bail!(
            "the API answered the PUT but the review step reads back {} — the hold did not take",
            held.map(|h| format!("holding {h:?}"))
                .unwrap_or_else(|| "with no hold".into())
        ),
        (None, None) => println!(
            "boss release: {} {branch} \"{}\" — released (was: {})\n  it boards at the next tick",
            short(&packet),
            title_of(&packet),
            already.unwrap_or_default()
        ),
        (None, Some(held)) => {
            bail!("the API answered the PUT but the review step still reads as held: {held:?}")
        }
    }
    Ok(())
}

// ----------------------------------------------------------------------
// `boss step complete` — the generic completion.
//
// WHY (backlog d7d28a54). The five verbs above cover five step kinds.
// Every other completion is a hand-built `boss-api PUT
// /api/jobs/{id}/steps/{sid}` whose body must be read off the packet
// first, and the page march is about to ask for ~94 of them (47 routes
// x `measure` + `file`). The three hazards that PUT carries are all in
// `boss-jobs/src/http/steps.rs`: `metadata` is REPLACED wholesale by
// the body's top-level keys (only `authority_role` and `human_only`
// carry forward), so a naive completion deletes the step's `procedure`
// and its `agent` block; an UNKNOWN field name is not refused but
// stored beside the real ones, so a name typed from memory reads as
// success and records nothing (retro 27fad542, class B); and a 204 is
// a claim, not a fact.
// ----------------------------------------------------------------------

/// A field the caller supplied, with its value already off the shell —
/// from `--field name=value` or read from `--field-file name=PATH`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Given {
    pub name: String,
    pub value: String,
}

/// `name=rest`, split on the FIRST `=`: a value carries `=` of its own
/// (a branch, a query, a probe line), and a name never does.
pub(crate) fn parse_pair(flag: &str, raw: &str) -> Result<(String, String), String> {
    match raw.split_once('=') {
        Some((name, value)) if !name.trim().is_empty() => {
            Ok((name.trim().to_string(), value.to_string()))
        }
        _ => Err(format!(
            "{flag} takes `name=value` (everything after the first `=` is the value) — got {raw:?}"
        )),
    }
}

/// The two flags read into one ordered list. The shell is removed here
/// and nowhere else: an argv value is trimmed, a file's bytes lose only
/// the editor's final newline.
pub(crate) fn given_values(fields: &[String], files: &[String]) -> Result<Vec<Given>> {
    let mut out = Vec::new();
    for raw in fields {
        let (name, value) = parse_pair("--field", raw).map_err(|e| anyhow!("{e}"))?;
        let value = value.trim().to_string();
        if value.is_empty() {
            // The one artifact of the 2376b89e defect that is
            // unambiguous, refused the way `prose::text_or_file`
            // refuses it and naming this flag's own file door.
            bail!(
                "--field {name}= is empty — nothing would be recorded. If a command \
                 substitution ate it, quote the value with SINGLE quotes or pass it through \
                 `--field-file {name}=<PATH>`: backticks inside double quotes are run by the \
                 shell before this verb sees them."
            );
        }
        out.push(Given { name, value });
    }
    for raw in files {
        let (name, path) = parse_pair("--field-file", raw).map_err(|e| anyhow!("{e}"))?;
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("--field-file {name}={path}: reading it"))?;
        let value = text.trim_end().to_string();
        if value.trim().is_empty() {
            bail!("--field-file {name}={path} is empty — nothing would be recorded");
        }
        out.push(Given { name, value });
    }
    Ok(out)
}

/// The row's contract, rendered for a refusal: what it declares, with
/// each field's type and whether it is required.
pub(crate) fn declares(fields: &[Field]) -> String {
    if fields.is_empty() {
        return "nothing".to_string();
    }
    fields
        .iter()
        .map(|f| {
            format!(
                "{} ({}{})",
                f.name,
                if f.required { "required " } else { "" },
                f.field_type
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// One value, typed by what the ROW declares rather than by what argv
/// carries. Everything reaching a verb through argv is a string; a
/// `number` field given `"3"` would be stored as a string and refused
/// at done by `validate_field_type`, one round trip later and in the
/// API's words rather than this flag's.
pub(crate) fn coerce(field: &Field, raw: &str) -> Result<Value, String> {
    let wrong = |want: &str| {
        format!(
            "`{}` is declared `{}` — {raw:?} is not {want}",
            field.name, field.field_type
        )
    };
    if let Some(allowed) = enum_values(&field.field_type) {
        return if allowed.contains(&raw) {
            Ok(json!(raw))
        } else {
            Err(format!(
                "`{raw}` is not a value `{}` declares — one of [{}]",
                field.name,
                allowed.join(", ")
            ))
        };
    }
    match field.field_type.as_str() {
        "number" => raw
            .parse::<f64>()
            .ok()
            .filter(|n| n.is_finite())
            .map(|n| json!(n))
            .ok_or_else(|| wrong("a number")),
        "integer" => raw
            .parse::<i64>()
            .map(|n| json!(n))
            .map_err(|_| wrong("an integer")),
        "boolean" => match raw {
            "true" => Ok(json!(true)),
            "false" => Ok(json!(false)),
            _ => Err(wrong("`true` or `false`")),
        },
        // A structured field's value is JSON, and JSON on a command
        // line is quoting the shell will fight: the refusal names the
        // file door, which is where a questions array belongs anyway.
        kind @ ("array" | "object") => {
            let value: Value = serde_json::from_str(raw).map_err(|e| {
                format!(
                    "`{}` is declared `{kind}` and its value must be JSON — {e}. JSON through \
                     argv is a quoting fight: write it to a file and pass `--field-file {}=<PATH>`",
                    field.name, field.name
                )
            })?;
            let ok = if kind == "array" {
                value.is_array()
            } else {
                value.is_object()
            };
            if ok {
                Ok(value)
            } else {
                Err(wrong(&format!("a JSON {kind}")))
            }
        }
        // string / uri / date / date-time, and any type spec this
        // version does not know: the text as given. The registry's own
        // validator below judges it, so an unknown spec is not guessed
        // at here.
        _ => Ok(json!(raw)),
    }
}

/// What the completion writes, decided against the step's declared
/// fields. THE UNDECLARED KEY IS THE HAZARD: `update_step` merges the
/// body over the step and stores a name no field declares, so a typo
/// answers 204 and records an annotation nobody reads. Refused here, by
/// name, naming what the row does declare.
pub(crate) fn field_writes(step: &Value, given: &[Given]) -> Result<Map<String, Value>, String> {
    field_writes_against(step, given, Vec::new())
}

/// [`field_writes`] with the step KIND's fields folded in — what the
/// API actually judges against.
pub(crate) fn field_writes_against(
    step: &Value,
    given: &[Given],
    kind_fields: Vec<Field>,
) -> Result<Map<String, Value>, String> {
    let declared = with_kind_fields(declared_fields(step), kind_fields);
    let mut writes = Map::new();
    for g in given {
        let field = declared.iter().find(|f| f.name == g.name).ok_or_else(|| {
            format!(
                "this step's row declares no `{}` field — it declares: {}. The API would not \
                 refuse it: an undeclared key is STORED beside the real ones and answers 204, \
                 so a name typed from memory reads as success and records nothing (retro \
                 27fad542)",
                g.name,
                declares(&declared)
            )
        })?;
        if writes.contains_key(&g.name) {
            return Err(format!(
                "`{}` was given twice — one value would silently win",
                g.name
            ));
        }
        writes.insert(g.name.clone(), coerce(field, &g.value)?);
    }
    Ok(writes)
}

/// The completion judged by the SERVER'S OWN rule before the round
/// trip: `StepRegistry::validate_authored_fields`, the very function
/// `update_step` runs at done (one definition, not a restatement of it
/// — the move `boss job list --where` makes with the containment
/// parser). So a missing required field is refused HERE, with the list,
/// rather than as a 422 the caller then guesses against.
pub(crate) fn contract_check(step: &Value, metadata: &Value) -> Result<(), String> {
    let declared: Vec<boss_core::job::StepField> =
        serde_json::from_value(step.get("fields").cloned().unwrap_or_else(|| json!([])))
            .unwrap_or_default();
    boss_jobs::step_registry::StepRegistry::validate_authored_fields(&declared, metadata).map_err(
        |errors| {
            format!(
                "this completion is not the shape the row declares:\n{}\n  the row declares: {}\
                 \n  supply each with `--field <name>=<value>`, or `--field-file <name>=<PATH>` \
                 for a long body",
                errors
                    .iter()
                    .map(|e| format!("  - {e}"))
                    .collect::<Vec<_>>()
                    .join("\n"),
                declares(&declared_fields(step))
            )
        },
    )
}

/// A `draft-design` completion names a design; the DESIGN must name the
/// packet back, or the link closes nothing.
///
/// THE DEFECT (backlog 2c7dd4ab, hit live 2026-09-20). "This design
/// answers that packet" has TWO representations and only one is
/// load-bearing:
///
/// 1. the declared `design-doc.answers` job edge, which
///    `complete-feedback-design-review-on-design-review-decided`
///    follows to complete the packet's `design-review`;
/// 2. the `design_id` field on `draft-design`, which is what completing
///    that step asks for.
///
/// Setting (2) LOOKS like linking the design. The rule cannot follow
/// it. So a hand-linked design is decided, out of the reviewer's queue,
/// and its packet waits forever — which is exactly what happened: David
/// decided design `f2cdff23`, saw it leave his queue, and `6c2eba00`
/// still read `design-review: ready` an hour later. He reasonably
/// concluded his review had been lost. It had not; the two records
/// simply disagreed and nothing reconciled them.
///
/// `boss design --answers` writes BOTH — the edge at filing, and this
/// step's completion when the route has one. This refusal is what
/// stops the other path from producing half of it in silence.
pub(crate) fn design_link_check(packet_id: &str, design: &Value) -> Result<(), String> {
    let answers = crate::design::answers_edge(design);
    let short = &packet_id[..8.min(packet_id.len())];
    let design_short = design
        .get("id")
        .and_then(Value::as_str)
        .map(|i| i[..8.min(i.len())].to_string())
        .unwrap_or_else(|| "?".into());
    match answers {
        Some(a) if a == packet_id => Ok(()),
        Some(other) => Err(format!(
            "design {design_short} answers {} , not {short}. Linking it here would \
             record a second, different claim about what this design decides — and the \
             rule that closes a packet follows the design's OWN edge, so {short} would \
             wait forever while the other packet closed.",
            &other[..8.min(other.len())]
        )),
        None => Err(format!(
            "design {design_short} carries no `answers` edge, so completing this step \
             would link it in a way NOTHING follows: the rule that closes a packet's \
             design-review reads the design's declared edge, not this field. The design \
             would be decided and out of the reviewer's queue while {short} waited \
             forever (backlog 2c7dd4ab).\n  \
             File it through the door that writes both: `boss design <title> --answers \
             {short} ...`. For a design that already exists, record the edge on it \
             first — `PATCH /api/jobs/{design_short}/metadata` with \
             {{\"answers\": \"{packet_id}\"}} — then complete this step."
        )),
    }
}

pub(crate) async fn complete(
    wire: &Wire,
    packet_ref: &str,
    slug: &str,
    given: &[Given],
) -> Result<()> {
    let packet = wire.resolve(packet_ref, None).await?;
    let step = open_step(&packet, slug).map_err(|e| anyhow!("{e}"))?;
    // The kind's own fields, from the registry the API validates
    // against. Best-effort: if the read fails the verb falls back to
    // the row alone, which is how it behaved before — a door that
    // cannot reach the registry should be no worse than it was, not
    // refuse outright.
    let kind = step.get("kind").and_then(Value::as_str).unwrap_or_default();
    let from_kind = match wire.step_types().await {
        Ok(types) => kind_fields(&types, kind),
        Err(_) => Vec::new(),
    };
    let writes = field_writes_against(step, given, from_kind)
        .map_err(|e| anyhow!("{} `{slug}`: {e}", short(&packet)))?;
    // A `draft-design` completion names a design; the design must name
    // the packet back, or the link closes nothing (backlog 2c7dd4ab).
    // Checked here rather than left to the rule, because the rule's
    // silence is the whole defect: a hand-linked design is decided, out
    // of the reviewer's queue, and its packet waits forever.
    if slug == "draft-design"
        && let Some(design_ref) = writes.get("design_id").and_then(Value::as_str)
    {
        let design = wire.resolve(design_ref, None).await?;
        let packet_id = crate::envelope::job_id(&packet)
            .context("the packet has no id")?
            .to_string();
        design_link_check(&packet_id, &design)
            .map_err(|e| anyhow!("{} `{slug}`: {e}", short(&packet)))?;
    }
    // Merged, not replaced — the step keeps its `procedure`, its
    // `agent` block and its audience — and the MERGED document is what
    // the registry judges, exactly as the API judges it after its own
    // merge.
    let body = completion(step, &writes);
    contract_check(step, &body["metadata"])
        .map_err(|e| anyhow!("{} `{slug}`: {e}", short(&packet)))?;
    let jid = crate::envelope::job_id(&packet).context("the packet has no id")?;
    let sid = step_id(step)?;
    wire.put_step(jid, sid, body).await?;

    let after = wire.packet(jid).await?;
    confirm_completed(step_after(&after, sid)?, &writes)?;
    let names = writes.keys().cloned().collect::<Vec<_>>().join(", ");
    println!(
        "boss step complete: {} \"{}\" — `{slug}` completed{}\n  {}",
        short(&packet),
        title_of(&packet),
        if names.is_empty() {
            String::new()
        } else {
            format!(" with {names}")
        },
        standing(&after)
    );
    Ok(())
}

// ----------------------------------------------------------------------
// `boss step release` — hand a claimed step back to its station.
//
// WHY (backlog e871febf). The clock rule
// `an-abandoned-step-is-reclaimed-when-its-run-died` frees a claimed
// step by asking whether the run NAMED ON IT died — and a step that
// carries no `agent_run` edge is, correctly, left alone forever:
// nothing names its executor, so nothing there can tell abandoned from
// busy. That is exactly the population the rule was written for. On
// 2026-09-19 a session orphaned fifteen runs, twelve of them holding a
// step that was built, gated green and never handed back; the edge
// only began being written that day (dd6d44b7), so those steps carry
// none. Measured on the live board 2026-09-22: of twenty claimed
// agent-workable steps, three carry no edge and have stood since the
// 19th. Small — which is why this is a VERB and not a sweep. A sweep
// is a hand that leaves nothing behind; a verb is a door the next
// operator finds when a step is stuck for a reason no rule covers.
//
// IT IS A HAND, AND IT SAYS SO. The clock rule records `reclaimed`
// with the death it measured. This records `released` with the reason
// a person gave, under a different key, because the two are different
// claims about the record: one is evidence, the other is testimony.
// ----------------------------------------------------------------------

/// Where a hand-release's evidence lands on the freed step —
/// deliberately not the clock rule's `reclaimed` (see the section
/// header): a reader must be able to tell which one freed the step.
pub(crate) const RELEASED_KEY: &str = "released";

/// The status a claim moves a step from, and the one it moves back to
/// — the pair `claim_step`'s CAS uses, so a release is a restoration
/// rather than a new routing opinion.
const CLAIMED_STATUS: &str = "active";
const WAITING_STATUS: &str = "ready";

/// The `slug` step, CLAIMED. A `ready` step is already where a release
/// puts one, and a terminal step is nobody's — writing the evidence on
/// either would record a takeover of work no one held.
pub(crate) fn releasable<'a>(packet: &'a Value, slug: &str) -> Result<&'a Value, String> {
    let step = crate::envelope::steps(packet)
        .into_iter()
        .find(|s| s.get("spec_slug").and_then(Value::as_str) == Some(slug))
        .ok_or_else(|| {
            format!(
                "packet {} ({}) has no `{slug}` step — {}",
                short(packet),
                kind_of(packet),
                standing(packet)
            )
        })?;
    if status_of(step) == CLAIMED_STATUS {
        Ok(step)
    } else {
        Err(format!(
            "packet {}'s `{slug}` is {} — a release hands back a step somebody HOLDS, and \
             nothing to release here; {}",
            short(packet),
            status_of(step),
            standing(packet)
        ))
    }
}

/// The metadata merge body: the run edge CLEARED, and one evidence
/// object saying who took the step back, from which run, and why.
///
/// THE NULL IS THE WHOLE POINT. `update_step` carries `agent_run`
/// forward when a PUT's metadata omits it (b91a2103), so the only door
/// that can clear the edge is `PATCH .../steps/{id}/metadata`, where an
/// explicit null deletes the key. Leaving a dead run named on a freed
/// step is not cosmetic: `agent-run-delivers-when-its-step-is-done`
/// follows that edge, so a step completed later would deliver onto a
/// run that is closed and dead.
pub(crate) fn release_patch(step: &Value, why: &str, by: Option<&str>, at: &str) -> Value {
    let from_run = step
        .pointer(&format!("/metadata/{}", boss_jobs::agent_runs::EDGE_KEY))
        .cloned()
        .unwrap_or(Value::Null);
    let mut body = Map::new();
    body.insert(boss_jobs::agent_runs::EDGE_KEY.to_string(), Value::Null);
    body.insert(
        RELEASED_KEY.to_string(),
        json!({
            "why": why.trim(),
            "by": by.map(|b| json!(b)).unwrap_or(Value::Null),
            "at": at,
            "from_run": from_run,
        }),
    );
    Value::Object(body)
}

/// The status write: `ready`, nobody's, and NO metadata. The PUT
/// replaces metadata wholesale, so omitting it keeps the stored
/// document — which the merge door has already cleared.
pub(crate) fn release_status() -> Value {
    json!({ "status": WAITING_STATUS, "assignee_id": Value::Null })
}

/// A 204 is a claim; the read-back is the fact. Four ways a release
/// half-happens, each named.
pub(crate) fn confirm_released(step: &Value, why: &str) -> Result<(), String> {
    if status_of(step) != WAITING_STATUS {
        return Err(format!(
            "the API answered but the step reads back {} — it was not released",
            status_of(step)
        ));
    }
    if let Some(who) = step.get("assignee_id").and_then(Value::as_str) {
        return Err(format!(
            "the step reads back `{WAITING_STATUS}` but is still assigned to {who} — the \
             station cannot hand out work somebody still holds"
        ));
    }
    if let Some(run) = step
        .pointer(&format!("/metadata/{}", boss_jobs::agent_runs::EDGE_KEY))
        .and_then(Value::as_str)
    {
        return Err(format!(
            "the step still names `{}` {run} — the merge door's null did not take, and the \
             next completion would deliver onto that run (b91a2103)",
            boss_jobs::agent_runs::EDGE_KEY
        ));
    }
    let recorded = step
        .pointer(&format!("/metadata/{RELEASED_KEY}/why"))
        .and_then(Value::as_str)
        .unwrap_or("");
    if recorded != why.trim() {
        return Err(format!(
            "the step reads back {recorded:?} as the reason — not what was sent, so the \
             record would not say why this step came free"
        ));
    }
    Ok(())
}

pub(crate) async fn release_step(
    wire: &Wire,
    packet_ref: &str,
    slug: &str,
    why: &str,
) -> Result<()> {
    if why.trim().is_empty() {
        bail!(
            "--why is blank — the reason is the whole artifact a release leaves, and a step \
             that came free with nothing behind it reads to the next operator exactly like \
             the stall it was"
        );
    }
    let packet = wire.resolve(packet_ref, None).await?;
    let step = releasable(&packet, slug).map_err(|e| anyhow!("{e}"))?;
    let jid = crate::envelope::job_id(&packet).context("the packet has no id")?;
    let sid = step_id(step)?;
    let held_by = holder(step);
    // The clear FIRST: if the status write then fails, the step is
    // still claimed and carries an annotation, which is a smaller
    // wrong than a step handed out while a dead run is named on it.
    // The record stamp through the one sanctioned wall source
    // (`boss_clock_client::wall_now`), never a second Utc::now() of
    // this module's own — the rule `no-wallclock` states.
    let patch = release_patch(
        step,
        why,
        wire.caller_id(),
        &boss_clock_client::wall_now().to_rfc3339(),
    );
    wire.patch_step_metadata(jid, sid, patch).await?;
    wire.put_step(jid, sid, release_status()).await?;

    let after = wire.packet(jid).await?;
    confirm_released(step_after(&after, sid)?, why).map_err(|e| anyhow!("{e}"))?;
    println!(
        "boss step release: {} \"{}\" — `{slug}` released (was held by {held_by})\n  why: {}\n  \
         {}",
        short(&packet),
        title_of(&packet),
        why.trim(),
        standing(&after)
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field(name: &str, field_type: &str, required: bool) -> Value {
        json!({ "name": name, "field_type": field_type, "required": required })
    }

    /// The backlog-item triage step, verbatim in shape from the live
    /// packet this car was briefed on (0d2e1655, workflow v2).
    fn backlog_triage(status: &str) -> Value {
        json!({
            "id": "11111111-1111-1111-1111-111111111111",
            "spec_slug": "triage",
            "status": status,
            "assignee_id": "claude@algedonic.dev",
            "fields": [
                field("disposition", "verify|design|build|duplicate|stale|decline", true),
                field("evidence", "string", true),
                field("context_md", "string", false),
                field("proposed", "string", false),
            ],
            "metadata": { "audience": { "role": "platform-admin" }, "authority_role": "platform-admin" },
        })
    }

    /// The user-feedback triage step (54f0ab33, workflow v1): a
    /// different enum, and `finding` where the backlog-item has
    /// `evidence`.
    fn feedback_triage() -> Value {
        json!({
            "id": "22222222-2222-2222-2222-222222222222",
            "spec_slug": "triage",
            "status": "ready",
            "fields": [
                field("disposition", "reproduce|design|build|duplicate|needs-info|decline", true),
                field("finding", "string", false),
            ],
            "metadata": { "authority_role": "platform-admin" },
        })
    }

    fn step(slug: &str, status: &str) -> Value {
        json!({ "id": format!("s-{slug}"), "spec_slug": slug, "status": status,
                "metadata": { "authority_role": "platform-admin" } })
    }

    fn packet(kind: &str, steps: Vec<Value>) -> Value {
        json!({
            "id": "0d2e1655-02c0-47d1-942a-5c8ae661f27f",
            "kind": kind,
            "status": "open",
            "title": "Retro: ten hand-built step PUTs",
            "metadata": {},
            "steps": steps,
        })
    }

    // ------------------------------------------------------------------
    // Reading the row
    // ------------------------------------------------------------------

    #[test]
    fn the_declared_fields_are_read_off_the_step_in_the_rows_order() {
        let fields = declared_fields(&backlog_triage("ready"));
        assert_eq!(
            fields.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(),
            vec!["disposition", "evidence", "context_md", "proposed"]
        );
        assert!(fields[0].required && fields[1].required && !fields[2].required);
        assert!(declared_fields(&json!({ "metadata": {} })).is_empty());
    }

    #[test]
    fn an_enum_is_the_pipe_spelling_and_a_scalar_is_not_one() {
        assert_eq!(
            enum_values("verify|design|build"),
            Some(vec!["verify", "design", "build"])
        );
        assert_eq!(enum_values("string"), None);
    }

    // ------------------------------------------------------------------
    // Which step
    // ------------------------------------------------------------------

    /// The refusal for a wrong step names where the packet IS, with
    /// its holder, so nobody runs `boss job get` to learn it.
    #[test]
    fn a_packet_not_at_triage_is_refused_naming_where_it_stands() {
        let p = packet(
            "backlog-item",
            vec![
                backlog_triage("completed"),
                json!({ "id": "s-build", "spec_slug": "build", "status": "ready",
                        "assignee_id": "claude@algedonic.dev", "metadata": {} }),
            ],
        );
        let err = open_step(&p, "triage").expect_err("triage is done");
        assert!(
            err.contains("0d2e1655") && err.contains("`triage` is completed"),
            "{err}"
        );
        assert!(
            err.contains("`build` (ready) held by claude@algedonic.dev"),
            "names the open step and who holds it: {err}"
        );
        // No such step at all: the kind is named, since that is the fix.
        let car = json!({ "id": "abcdef12-0000-0000-0000-000000000000", "kind": "ship-a-change",
                          "status": "open", "steps": [] });
        let err = open_step(&car, "triage").expect_err("no triage step");
        assert!(
            err.contains("ship-a-change") && err.contains("no `triage` step"),
            "{err}"
        );
    }

    #[test]
    fn standing_reads_the_open_steps_or_the_terminal() {
        let p = packet(
            "backlog-item",
            vec![
                step("triage", "completed"),
                json!({ "id": "s-r", "spec_slug": "design-review", "status": "ready",
                        "metadata": { "authority_role": "platform-admin" } }),
            ],
        );
        assert_eq!(
            standing(&p),
            "now at `design-review` (ready) held by role platform-admin (unassigned)"
        );
        let mut closed = packet("backlog-item", vec![step("triage", "completed")]);
        closed["status"] = json!("closed");
        closed["metadata"] = json!({ "outcome": "duplicate" });
        assert_eq!(standing(&closed), "closed — outcome duplicate");
        let nothing = packet("backlog-item", vec![step("triage", "pending")]);
        assert_eq!(
            standing(&nothing),
            "no step is open",
            "a step that is nobody's and not open is not 'at' anything"
        );
    }

    // ------------------------------------------------------------------
    // triage
    // ------------------------------------------------------------------

    #[test]
    fn a_triage_writes_the_declared_fields_and_only_those() {
        let w = triage_writes(
            &backlog_triage("ready"),
            "stale",
            "  re-measured: gone  ",
            None,
        )
        .expect("stale is declared");
        assert_eq!(w["disposition"], json!("stale"));
        assert_eq!(w["evidence"], json!("re-measured: gone"), "trimmed");
        assert_eq!(w.len(), 2, "no key the row did not declare: {w:?}");
    }

    /// The row decides the text field: user-feedback declares
    /// `finding`, not `evidence`, and its enum is a different set.
    #[test]
    fn the_text_field_and_the_enum_come_from_the_row_not_the_verb() {
        let w = triage_writes(&feedback_triage(), "reproduce", "seen twice", None)
            .expect("reproduce is in user-feedback's enum");
        assert_eq!(w["finding"], json!("seen twice"));
        assert!(w.get("evidence").is_none(), "{w:?}");
        // `stale` is a backlog-item route, not a user-feedback one.
        let err = triage_writes(&feedback_triage(), "stale", "x", None).expect_err("not declared");
        assert!(
            err.contains("`stale`")
                && err.contains("reproduce, design, build, duplicate, needs-info, decline"),
            "the refusal names the value and the row's whole enum: {err}"
        );
    }

    #[test]
    fn a_disposition_outside_the_enum_is_refused_naming_the_enum() {
        let err = triage_writes(&backlog_triage("ready"), "wontfix", "x", None)
            .expect_err("wontfix is not declared");
        assert!(
            err.contains("`wontfix`")
                && err.contains("[verify, design, build, duplicate, stale, decline]"),
            "{err}"
        );
    }

    #[test]
    fn blank_evidence_is_refused() {
        let err = triage_writes(&backlog_triage("ready"), "stale", "   ", None)
            .expect_err("blank evidence");
        assert!(
            err.contains("--evidence is blank") && err.contains("`evidence`"),
            "{err}"
        );
    }

    #[test]
    fn duplicate_requires_of_and_records_it_and_of_means_nothing_otherwise() {
        let err = triage_writes(&backlog_triage("ready"), "duplicate", "same as", None)
            .expect_err("no --of");
        assert!(
            err.contains("--of") && err.contains("duplicate_of"),
            "{err}"
        );
        let w = triage_writes(
            &backlog_triage("ready"),
            "duplicate",
            "same as",
            Some("236529aa-cf49-4c50-856b-889160e3d565"),
        )
        .expect("duplicate with --of");
        assert_eq!(
            w["duplicate_of"],
            json!("236529aa-cf49-4c50-856b-889160e3d565"),
            "the full id, as the one hand-written duplicate on record spells it (9e627310)"
        );
        let err = triage_writes(
            &backlog_triage("ready"),
            "stale",
            "x",
            Some("236529aa-cf49-4c50-856b-889160e3d565"),
        )
        .expect_err("--of with stale");
        assert!(err.contains("--of only accompanies `duplicate`"), "{err}");
    }

    /// A step whose row declares no `disposition`, or declares it as a
    /// bare string, cannot be completed by a verb that reads the enum
    /// — refused rather than guessed.
    #[test]
    fn a_row_without_the_fields_is_refused_naming_what_it_declares() {
        let bare = json!({ "spec_slug": "triage", "status": "ready",
                           "fields": [field("verdict", "string", true)], "metadata": {} });
        let err = triage_writes(&bare, "stale", "x", None).expect_err("no disposition");
        assert!(
            err.contains("no `disposition` field") && err.contains("verdict"),
            "{err}"
        );
        let untyped = json!({ "spec_slug": "triage", "status": "ready",
                              "fields": [field("disposition", "string", true), field("evidence", "string", true)],
                              "metadata": {} });
        let err = triage_writes(&untyped, "stale", "x", None).expect_err("not an enum");
        assert!(err.contains("not an enum"), "{err}");
        let no_text = json!({ "spec_slug": "triage", "status": "ready",
                              "fields": [field("disposition", "a|b", true)], "metadata": {} });
        let err = triage_writes(&no_text, "a", "x", None).expect_err("no text field");
        assert!(
            err.contains("evidence or finding") && err.contains("disposition"),
            "{err}"
        );
    }

    /// PATCH-on-PUT replaces `metadata` wholesale, and `authority_role`
    /// / `audience` live there: the completion carries them through.
    #[test]
    fn the_completion_lays_the_writes_over_the_steps_own_keys() {
        let step = backlog_triage("ready");
        let writes = triage_writes(&step, "build", "measured", None).unwrap();
        let body = completion(&step, &writes);
        assert_eq!(body["status"], json!("completed"));
        assert_eq!(body["metadata"]["authority_role"], json!("platform-admin"));
        assert_eq!(
            body["metadata"]["audience"]["role"],
            json!("platform-admin")
        );
        assert_eq!(body["metadata"]["disposition"], json!("build"));
        assert_eq!(body["metadata"]["evidence"], json!("measured"));
    }

    // ------------------------------------------------------------------
    // fold
    // ------------------------------------------------------------------

    fn design(review_status: &str, fold_status: &str, resolved: &[&str]) -> Value {
        let mut p = packet(
            "design-doc",
            vec![
                step("drafted", "completed"),
                json!({
                    "id": "s-review", "spec_slug": "review", "status": review_status,
                    "metadata": {
                        "authority_role": "platform-admin",
                        "questions": [
                            { "anchor": "order", "title": "Is the order right?", "proposal": "yes" },
                            { "anchor": "delete-bare-metal", "title": "Delete it?", "proposal": "yes" },
                        ],
                        "resolutions": resolved.iter().map(|a| json!({ "anchor": a, "decision": "yes" })).collect::<Vec<_>>(),
                    },
                }),
                json!({
                    "id": "s-fold", "spec_slug": "fold", "status": fold_status,
                    "fields": [field("fold_change", "string", true)],
                    "metadata": { "procedure": "State what CURRENT TRUTH gains" },
                }),
                json!({ "id": "s-pub", "spec_slug": "published", "status": "pending",
                        "metadata": { "outcome_kind": "completed" } }),
            ],
        );
        p["title"] = json!("Consolidation toward 1.0.0");
        p
    }

    #[test]
    fn a_fold_before_the_review_is_done_is_refused_naming_the_open_anchors() {
        let err = foldable(&design("ready", "pending", &["order"])).expect_err("review open");
        assert!(
            err.contains("review is ready")
                && err.contains("1 question(s) still open: delete-bare-metal"),
            "{err}"
        );
        assert!(
            !err.contains("order"),
            "an answered anchor is not listed: {err}"
        );
        // Answered but not completed: the step, not the anchors, is the fix.
        let err = foldable(&design(
            "active",
            "pending",
            &["order", "delete-bare-metal"],
        ))
        .expect_err("review not completed");
        assert!(
            err.contains("all answered") && err.contains("complete it"),
            "{err}"
        );
    }

    #[test]
    fn a_fold_at_its_ready_step_writes_fold_change_over_the_procedure() {
        let d = design("completed", "ready", &["order", "delete-bare-metal"]);
        let step = foldable(&d).expect("fold is ready");
        let writes = fold_writes(
            step,
            " docs/architecture-decisions.md gains a section ",
            None,
        )
        .unwrap();
        assert_eq!(
            writes["fold_change"],
            json!("docs/architecture-decisions.md gains a section")
        );
        let body = completion(step, &writes);
        assert_eq!(
            body["metadata"]["procedure"],
            json!("State what CURRENT TRUTH gains")
        );
        // Already folded: the refusal is the generic standing one.
        let err = foldable(&design("completed", "completed", &[])).expect_err("folded");
        assert!(err.contains("`fold` is completed"), "{err}");
    }

    #[test]
    fn a_blank_fold_change_and_a_row_without_the_field_are_refused() {
        let d = design("completed", "ready", &[]);
        let step = foldable(&d).unwrap();
        let err = fold_writes(step, " ", None).expect_err("blank");
        assert!(err.contains("--change is blank"), "{err}");
        let bare = json!({ "spec_slug": "fold", "status": "ready", "fields": [], "metadata": {} });
        let err = fold_writes(&bare, "x", None).expect_err("no fold_change");
        assert!(err.contains("no `fold_change` field"), "{err}");
    }

    /// `folded_into` arrives in a LATER design-doc version (backlog
    /// aaf85ca2), and in-flight packets stay pinned to the version they
    /// were admitted under — so this one verb has to complete a fold on
    /// a row that declares the field and on one that does not. The row
    /// decides, not the flag.
    #[test]
    fn the_fold_writes_where_it_landed_only_when_the_row_declares_it() {
        let with = json!({
            "spec_slug": "fold", "status": "ready", "metadata": {},
            "fields": [field("fold_change", "string", true),
                       field("folded_into", "string", true)],
        });
        let writes = fold_writes(
            &with,
            "a section on the VSM mapping",
            Some("  docs/architecture-decisions.md  "),
        )
        .expect("a row that declares it takes it");
        assert_eq!(
            writes["folded_into"],
            json!("docs/architecture-decisions.md")
        );

        // A row from the older version: the flag is simply not written,
        // rather than the verb refusing a packet it can still complete.
        let without = json!({
            "spec_slug": "fold", "status": "ready", "metadata": {},
            "fields": [field("fold_change", "string", true)],
        });
        let writes = fold_writes(
            &without,
            "same change",
            Some("docs/architecture-decisions.md"),
        )
        .expect("an older row still folds");
        assert!(
            !writes.contains_key("folded_into"),
            "a field the row does not declare must not be written: {writes:?}"
        );
    }

    /// "Nothing" is a legitimate fold, so the field takes `none` — but
    /// it does not take SILENCE, for the same reason --change does not.
    #[test]
    fn a_declared_folded_into_refuses_to_be_left_blank() {
        let with = json!({
            "spec_slug": "fold", "status": "ready", "metadata": {},
            "fields": [field("fold_change", "string", true),
                       field("folded_into", "string", true)],
        });
        let err = fold_writes(&with, "a real change", None).expect_err("absent");
        assert!(err.contains("--folded-into"), "{err}");
        let err = fold_writes(&with, "a real change", Some("   ")).expect_err("blank");
        assert!(err.contains("--folded-into"), "{err}");
    }

    // ------------------------------------------------------------------
    // hold / release
    // ------------------------------------------------------------------

    fn car(review_status: &str, review_md: Value) -> Value {
        json!({
            "id": "c6bd173e-3dc9-426f-8fff-866a3b2a6117",
            "kind": "ship-a-change",
            "status": "open",
            "title": "A car lands where its change goes live",
            "metadata": { "branch": "fix/held" },
            "steps": [
                step("gate", "completed"),
                json!({ "id": "s-review", "spec_slug": "review", "title": "Open for review",
                        "status": review_status, "metadata": review_md }),
                step("proven", "pending"),
            ],
        })
    }

    /// The marker in the shape every reader reads (`stranded::hold_reason`
    /// — the conductor's `parked_ready`, the loading-dock row's
    /// `metadata_unmarked`, the yard's held lane, `boss orient`): a
    /// non-blank string on the REVIEW step, every other key kept.
    #[test]
    fn a_hold_is_the_marker_the_readers_read_and_keeps_the_steps_keys() {
        let c = car(
            "ready",
            json!({ "authority_role": "platform-admin", "procedure": "Left ready" }),
        );
        let review = holdable(&c).expect("parked");
        let md = hold_metadata(review, " waiting on a kubectl delete ");
        assert_eq!(
            boss_jobs::stranded::hold_reason(&md).as_deref(),
            Some("waiting on a kubectl delete"),
            "the one reader the conductor uses must read it back"
        );
        assert_eq!(md["authority_role"], json!("platform-admin"));
        assert_eq!(md["procedure"], json!("Left ready"));
        let steps: Vec<boss_core::job::Step> = vec![serde_json::from_value(json!({
            "title": "Open for review", "spec_slug": "review", "status": "ready", "metadata": md,
        }))
        .unwrap()];
        assert_eq!(
            boss_jobs::yard::car_hold_reason(&steps).as_deref(),
            Some("waiting on a kubectl delete"),
            "and so must the yard's held lane"
        );
    }

    /// Released = the KEY REMOVED. `hold: false` reads as released to
    /// `hold_reason` but as "held, then released" to every other eye.
    #[test]
    fn a_release_removes_the_key_rather_than_falsing_it() {
        let c = car(
            "ready",
            json!({ "authority_role": "platform-admin", "hold": "x" }),
        );
        let md = release_metadata(holdable(&c).unwrap());
        assert!(md.get("hold").is_none(), "{md}");
        assert_eq!(md["authority_role"], json!("platform-admin"));
        assert!(boss_jobs::stranded::hold_reason(&md).is_none());
    }

    #[test]
    fn a_car_past_review_or_without_one_cannot_be_held() {
        let landed = car("completed", json!({ "pr_url": "x" }));
        let err = holdable(&landed).expect_err("review done");
        assert!(
            err.contains("c6bd173e") && err.contains("review is completed"),
            "{err}"
        );
        let not_a_car = packet("backlog-item", vec![step("triage", "ready")]);
        let err = holdable(&not_a_car).expect_err("no review");
        assert!(
            err.contains("backlog-item") && err.contains("no review step"),
            "{err}"
        );
    }

    // ------------------------------------------------------------------
    // The wire, against an in-memory jobs API: a stub that serves the
    // list, the packet, and applies a step PUT the way `update_step`
    // does (PATCH over the step; `metadata` replaced wholesale), so
    // the read-back the verb insists on is a real one.
    // ------------------------------------------------------------------

    use std::sync::{Arc, Mutex};

    struct Stub {
        packets: Arc<Mutex<Vec<Value>>>,
        puts: Arc<Mutex<Vec<(String, Value)>>>,
        /// Answer every PUT 204 and store nothing — the silent no-op.
        drop_puts: Arc<std::sync::atomic::AtomicBool>,
        base: String,
    }

    async fn stub(packets: Vec<Value>) -> Stub {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let store = Arc::new(Mutex::new(packets));
        let puts: Arc<Mutex<Vec<(String, Value)>>> = Arc::new(Mutex::new(Vec::new()));
        let drop_puts = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (s, p, d) = (store.clone(), puts.clone(), drop_puts.clone());
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    break;
                };
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                let mut want = None;
                loop {
                    match sock.read(&mut chunk).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => buf.extend_from_slice(&chunk[..n]),
                    }
                    if want.is_none()
                        && let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n")
                    {
                        let head = String::from_utf8_lossy(&buf[..end]).to_ascii_lowercase();
                        let len = head
                            .lines()
                            .find_map(|l| l.strip_prefix("content-length:"))
                            .and_then(|v| v.trim().parse::<usize>().ok())
                            .unwrap_or(0);
                        want = Some(end + 4 + len);
                    }
                    if want.is_some_and(|w| buf.len() >= w) {
                        break;
                    }
                }
                let text = String::from_utf8_lossy(&buf).into_owned();
                let mut words = text.split_whitespace();
                let method = words.next().unwrap_or("GET").to_string();
                let target = words.next().unwrap_or("/").to_string();
                let body_text = text.split("\r\n\r\n").nth(1).unwrap_or("");
                let dropping = d.load(std::sync::atomic::Ordering::SeqCst);
                let (status, body) = route(&s, &p, dropping, &method, &target, body_text);
                let resp = format!(
                    "HTTP/1.1 {status}\r\ncontent-type: application/json\r\n\
                     content-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.shutdown().await;
            }
        });
        Stub {
            packets: store,
            puts,
            drop_puts,
            base: format!("http://{addr}"),
        }
    }

    fn route(
        store: &Mutex<Vec<Value>>,
        puts: &Mutex<Vec<(String, Value)>>,
        dropping: bool,
        method: &str,
        target: &str,
        body: &str,
    ) -> (&'static str, String) {
        let (path, query) = target.split_once('?').unwrap_or((target, ""));
        let param = |k: &str| -> Option<String> {
            query
                .split('&')
                .find_map(|p| p.strip_prefix(&format!("{k}=")))
                .map(str::to_string)
        };
        let mut packets = store.lock().unwrap();
        match (method, path.trim_start_matches("/api/jobs")) {
            ("GET", "") => {
                let kind = param("kind");
                let rows: Vec<Value> = packets
                    .iter()
                    .filter(|p| kind.as_deref().is_none_or(|k| p["kind"] == k))
                    .cloned()
                    .collect();
                (
                    "200 OK",
                    json!({ "data": rows, "total": rows.len() }).to_string(),
                )
            }
            ("GET", rest) => {
                let id = rest.trim_start_matches('/');
                match packets.iter().find(|p| p["id"] == id) {
                    Some(p) => ("200 OK", p.to_string()),
                    None => ("404 Not Found", "no such job".into()),
                }
            }
            ("PUT", rest) => {
                let mut parts = rest.trim_start_matches('/').split('/');
                let (jid, _, sid) = (
                    parts.next().unwrap_or(""),
                    parts.next(),
                    parts.next().unwrap_or(""),
                );
                let sent: Value = serde_json::from_str(body).unwrap_or(Value::Null);
                puts.lock().unwrap().push((rest.to_string(), sent.clone()));
                if dropping {
                    return ("204 No Content", String::new());
                }
                let Some(p) = packets.iter_mut().find(|p| p["id"] == jid) else {
                    return ("404 Not Found", "no such job".into());
                };
                let Some(step) = p["steps"]
                    .as_array_mut()
                    .and_then(|s| s.iter_mut().find(|s| s["id"] == sid))
                else {
                    return ("404 Not Found", "step not found".into());
                };
                // The frozen-row rule (http/steps.rs): a terminal step's
                // metadata is immutable, and saying so is the point.
                if matches!(step["status"].as_str(), Some("completed" | "skipped"))
                    && sent.get("metadata").is_some_and(|m| *m != step["metadata"])
                {
                    return ("409 Conflict", r#"{"error":"step is terminal"}"#.into());
                }
                // THE SERVER CARRIES THE RUN EDGE FORWARD when a PUT's
                // metadata omits it (b91a2103, pinned by
                // `the_run_edge_survives_a_metadata_put`). Modelled
                // here so a release that tries to clear the edge
                // through THIS door fails the test exactly as it fails
                // live, instead of passing against a stub that is
                // kinder than the API.
                let carried = step["metadata"]
                    .get(boss_jobs::agent_runs::EDGE_KEY)
                    .cloned();
                if let Some(obj) = sent.as_object() {
                    for (k, v) in obj {
                        step[k] = v.clone();
                    }
                }
                if let Some(run) = carried
                    && let Some(md) = step["metadata"].as_object_mut()
                {
                    md.entry(boss_jobs::agent_runs::EDGE_KEY.to_string())
                        .or_insert(run);
                }
                ("204 No Content", String::new())
            }
            // The step's MERGE door: one key at a time, an explicit
            // null deletes.
            ("PATCH", rest) if rest.ends_with("/metadata") && rest.contains("/steps/") => {
                let trimmed = rest.trim_end_matches("/metadata");
                let mut parts = trimmed.trim_start_matches('/').split('/');
                let (jid, _, sid) = (
                    parts.next().unwrap_or(""),
                    parts.next(),
                    parts.next().unwrap_or(""),
                );
                let sent: Value = serde_json::from_str(body).unwrap_or(Value::Null);
                puts.lock().unwrap().push((rest.to_string(), sent.clone()));
                if dropping {
                    return ("204 No Content", String::new());
                }
                let Some(p) = packets.iter_mut().find(|p| p["id"] == jid) else {
                    return ("404 Not Found", "no such job".into());
                };
                let Some(step) = p["steps"]
                    .as_array_mut()
                    .and_then(|s| s.iter_mut().find(|s| s["id"] == sid))
                else {
                    return ("404 Not Found", "step not found".into());
                };
                if !step["metadata"].is_object() {
                    step["metadata"] = json!({});
                }
                if let (Some(md), Some(obj)) = (step["metadata"].as_object_mut(), sent.as_object())
                {
                    for (k, v) in obj {
                        if v.is_null() {
                            md.remove(k);
                        } else {
                            md.insert(k.clone(), v.clone());
                        }
                    }
                }
                ("204 No Content", String::new())
            }
            _ => ("404 Not Found", "unrouted".into()),
        }
    }

    fn named() -> Option<identity::Caller> {
        Some(identity::Caller {
            id: "claude@algedonic.dev".into(),
            source: identity::Source::Env,
        })
    }

    /// The whole verb, end to end: resolves an 8-char prefix among the
    /// open packets, resolves `--of` to the full id, PUTs the
    /// completion, reads the packet back and confirms it.
    #[tokio::test]
    async fn triage_completes_the_step_and_records_the_resolved_original() {
        let original = json!({
            "id": "236529aa-cf49-4c50-856b-889160e3d565", "kind": "backlog-item", "status": "open",
            "title": "the original", "metadata": {}, "steps": [step("triage", "ready")],
        });
        let s = stub(vec![
            packet(
                "backlog-item",
                vec![step("filed", "completed"), backlog_triage("ready")],
            ),
            original,
        ])
        .await;
        let wire = Wire::at(s.base.clone(), named());
        triage(
            &wire,
            "0d2e1655",
            "duplicate",
            "same defect, older packet",
            Some("236529aa"),
        )
        .await
        .expect("completes");
        let puts = s.puts.lock().unwrap();
        assert_eq!(puts.len(), 1, "one step PUT: {puts:?}");
        let (path, body) = &puts[0];
        assert_eq!(
            path,
            "/0d2e1655-02c0-47d1-942a-5c8ae661f27f/steps/11111111-1111-1111-1111-111111111111"
        );
        assert_eq!(body["status"], json!("completed"));
        assert_eq!(body["metadata"]["disposition"], json!("duplicate"));
        assert_eq!(
            body["metadata"]["evidence"],
            json!("same defect, older packet")
        );
        assert_eq!(
            body["metadata"]["duplicate_of"],
            json!("236529aa-cf49-4c50-856b-889160e3d565"),
            "the full id, resolved — never the eight characters typed"
        );
        assert_eq!(body["metadata"]["authority_role"], json!("platform-admin"));
    }

    /// The refusals happen BEFORE the write: a wrong disposition, a
    /// packet not at triage, and an unnamed actor each leave the stub
    /// with no PUT at all.
    #[tokio::test]
    async fn every_triage_refusal_happens_before_any_write() {
        let s = stub(vec![packet(
            "backlog-item",
            vec![step("filed", "completed"), backlog_triage("ready")],
        )])
        .await;
        let wire = Wire::at(s.base.clone(), named());
        let err = triage(&wire, "0d2e1655", "wontfix", "x", None)
            .await
            .expect_err("wontfix");
        assert!(
            err.to_string()
                .contains("[verify, design, build, duplicate, stale, decline]"),
            "{err}"
        );
        let err = triage(&wire, "0d2e1655", "duplicate", "x", None)
            .await
            .expect_err("no --of");
        assert!(err.to_string().contains("--of"), "{err}");
        let err = triage(
            &wire,
            "0d2e1655",
            "duplicate",
            "x",
            Some("ffffffff-0000-0000-0000-000000000000"),
        )
        .await
        .expect_err("--of names nothing");
        assert!(err.to_string().contains("--of ffffffff"), "{err}");
        // Backlog 5083d6f5: a write nobody named does not go out.
        let unnamed = Wire::at(s.base.clone(), None);
        let err = triage(&unnamed, "0d2e1655", "stale", "gone", None)
            .await
            .expect_err("unnamed");
        assert!(err.to_string().contains(identity::ACTOR_ENV), "{err}");
        assert!(
            s.puts.lock().unwrap().is_empty(),
            "no refusal reached the socket"
        );
        // And the one that is not a refusal: already triaged.
        s.packets.lock().unwrap()[0]["steps"][1]["status"] = json!("completed");
        let err = triage(&wire, "0d2e1655", "stale", "gone", None)
            .await
            .expect_err("done");
        assert!(err.to_string().contains("`triage` is completed"), "{err}");
        assert!(s.puts.lock().unwrap().is_empty());
    }

    /// A 204 that changed nothing is a failure, not "triaged". The stub
    /// here answers the PUT and stores nothing — the write-once class
    /// `boss job patch` was built to catch (a07cfddd), and the class
    /// three "repaired" receipts fell into (09576fab).
    #[tokio::test]
    async fn a_put_the_packet_does_not_reflect_is_a_failure() {
        let s = stub(vec![packet("backlog-item", vec![backlog_triage("ready")])]).await;
        s.drop_puts.store(true, std::sync::atomic::Ordering::SeqCst);
        let wire = Wire::at(s.base.clone(), named());
        let err = triage(&wire, "0d2e1655", "stale", "gone", None)
            .await
            .expect_err("a dropped write must not read as triaged");
        assert!(
            err.to_string().contains("reads back as ready"),
            "the failure names what the step holds instead: {err}"
        );
        assert_eq!(
            s.puts.lock().unwrap().len(),
            1,
            "the PUT was sent, and answered"
        );
    }

    #[tokio::test]
    async fn fold_completes_the_ready_fold_and_prints_the_terminal() {
        let s = stub(vec![design(
            "completed",
            "ready",
            &["order", "delete-bare-metal"],
        )])
        .await;
        let wire = Wire::at(s.base.clone(), named());
        fold(
            &wire,
            "0d2e1655",
            "architecture-decisions.md gains a section",
            None,
        )
        .await
        .expect("folds");
        let puts = s.puts.lock().unwrap();
        assert_eq!(puts.len(), 1);
        assert_eq!(
            puts[0].1["metadata"]["fold_change"],
            json!("architecture-decisions.md gains a section")
        );
        assert_eq!(
            puts[0].1["metadata"]["procedure"],
            json!("State what CURRENT TRUTH gains")
        );
    }

    #[tokio::test]
    async fn fold_refuses_an_undecided_review_before_any_write() {
        let s = stub(vec![design("ready", "pending", &[])]).await;
        let wire = Wire::at(s.base.clone(), named());
        let err = fold(&wire, "0d2e1655", "x", None)
            .await
            .expect_err("review open");
        assert!(
            err.to_string().contains("order, delete-bare-metal"),
            "{err}"
        );
        assert!(s.puts.lock().unwrap().is_empty());
        // Not a design-doc at all: the kind filter finds nothing.
        let s = stub(vec![packet("backlog-item", vec![step("triage", "ready")])]).await;
        let wire = Wire::at(s.base.clone(), named());
        let err = fold(&wire, "0d2e1655", "x", None)
            .await
            .expect_err("not a design");
        assert!(err.to_string().contains("no job matches"), "{err}");
    }

    /// Hold by branch, release by id prefix — `boss prove`'s resolver
    /// — and the review step reads back held, then not.
    #[tokio::test]
    async fn hold_then_release_write_the_marker_on_the_review_step() {
        let s = stub(vec![car(
            "ready",
            json!({ "authority_role": "platform-admin" }),
        )])
        .await;
        let wire = Wire::at(s.base.clone(), named());
        hold(&wire, "fix/held", Some("waiting on a kubectl delete"))
            .await
            .expect("holds");
        {
            let puts = s.puts.lock().unwrap();
            assert_eq!(puts.len(), 1);
            assert_eq!(
                puts[0].0,
                "/c6bd173e-3dc9-426f-8fff-866a3b2a6117/steps/s-review"
            );
            assert_eq!(
                puts[0].1["metadata"]["hold"],
                json!("waiting on a kubectl delete")
            );
            assert!(
                puts[0].1.get("status").is_none(),
                "a hold does not touch status"
            );
        }
        assert_eq!(
            boss_jobs::stranded::hold_reason(&s.packets.lock().unwrap()[0]["steps"][1]["metadata"])
                .as_deref(),
            Some("waiting on a kubectl delete")
        );
        hold(&wire, "c6bd173e", None).await.expect("releases");
        let md = s.packets.lock().unwrap()[0]["steps"][1]["metadata"].clone();
        assert!(md.get("hold").is_none(), "{md}");
        assert_eq!(md["authority_role"], json!("platform-admin"));
        // Releasing again is a no-op that says so, not a write.
        hold(&wire, "fix/held", None)
            .await
            .expect("nothing to release");
        assert_eq!(s.puts.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn hold_refuses_a_blank_reason_a_landed_car_and_an_unknown_one_before_any_write() {
        let s = stub(vec![
            car("ready", json!({})),
            json!({ "id": "aaaaaaaa-0000-0000-0000-000000000000", "kind": "ship-a-change",
                    "status": "open", "title": "landed", "metadata": { "branch": "fix/landed" },
                    "steps": [json!({ "id": "s-r", "spec_slug": "review", "status": "completed", "metadata": {} })] }),
        ])
        .await;
        let wire = Wire::at(s.base.clone(), named());
        let err = hold(&wire, "fix/held", Some("  "))
            .await
            .expect_err("blank");
        assert!(err.to_string().contains("--reason is blank"), "{err}");
        let err = hold(&wire, "fix/landed", Some("x"))
            .await
            .expect_err("landed");
        assert!(err.to_string().contains("review is completed"), "{err}");
        let err = hold(&wire, "fix/nothing", Some("x"))
            .await
            .expect_err("unknown");
        assert!(
            err.to_string()
                .contains("no ship-a-change car for \"fix/nothing\""),
            "{err}"
        );
        assert!(s.puts.lock().unwrap().is_empty());
    }

    // ------------------------------------------------------------------
    // `boss step complete` — the generic completion (backlog d7d28a54)
    // ------------------------------------------------------------------

    /// The page-audit `measure` step, in the shape the live row
    /// declares it (infra/platform/workflows/page-audit.toml): three
    /// required markdown bodies, and a metadata block carrying the
    /// procedure and the agent's own keys — the keys a wholesale
    /// metadata PUT deletes.
    fn measure_step(status: &str) -> Value {
        json!({
            "id": "33333333-3333-3333-3333-333333333333",
            "spec_slug": "measure",
            "status": status,
            "assignee_id": "claude@algedonic.dev",
            "fields": [
                field("controls_md", "string", true),
                field("needs_md", "string", true),
                field("gaps_md", "string", true),
            ],
            "metadata": {
                "human_only": false,
                "authority_role": "platform-admin",
                "procedure": "Read the PAGE and the DEPARTMENT, and write the difference.",
                "agent_profile": "analyst",
            },
        })
    }

    fn given(pairs: &[(&str, &str)]) -> Vec<Given> {
        pairs
            .iter()
            .map(|(n, v)| Given {
                name: (*n).to_string(),
                value: (*v).to_string(),
            })
            .collect()
    }

    /// A value carries `=` of its own — a query, a branch, a probe line
    /// — so the split is on the FIRST one and never on the last.
    #[test]
    fn a_pair_splits_on_the_first_equals_and_a_nameless_one_is_refused() {
        assert_eq!(
            parse_pair("--field", "car=fix/a?x=1&y=2"),
            Ok(("car".to_string(), "fix/a?x=1&y=2".to_string()))
        );
        for bad in ["no-equals", "=value"] {
            let err = parse_pair("--field", bad).expect_err("refused");
            assert!(err.contains("--field takes `name=value`"), "{err}");
        }
    }

    /// HAZARD 2, the one the API does not guard: an undeclared key is
    /// stored beside the real fields and answers 204. Refused here BY
    /// NAME, and the refusal names what the row does declare so the
    /// next attempt is right.
    #[test]
    fn an_undeclared_field_is_refused_by_name_and_names_what_the_row_declares() {
        let err = field_writes(
            &measure_step("ready"),
            &given(&[("controls", "seven links")]),
        )
        .expect_err("refused");
        assert!(err.contains("no `controls` field"), "{err}");
        assert!(
            err.contains("controls_md (required string)") && err.contains("gaps_md"),
            "names the declared contract: {err}"
        );
        // And a step whose row declares nothing at all says so rather
        // than printing an empty list.
        let bare = json!({ "id": "s", "spec_slug": "build", "status": "ready", "metadata": {} });
        let err = field_writes(&bare, &given(&[("anything", "x")])).expect_err("refused");
        assert!(err.contains("it declares: nothing"), "{err}");
    }

    /// The same value twice is a silent overwrite, whichever flag it
    /// came from.
    #[test]
    fn the_same_field_given_twice_is_refused() {
        let err = field_writes(
            &measure_step("ready"),
            &given(&[("gaps_md", "one"), ("gaps_md", "two")]),
        )
        .expect_err("refused");
        assert!(err.contains("`gaps_md` was given twice"), "{err}");
    }

    /// Everything through argv is a string; the ROW decides the type.
    /// A `number` stored as `"3"` passes this verb and is refused at
    /// done by the API, one round trip later.
    #[test]
    fn a_value_is_typed_by_what_the_row_declares() {
        let f = |t: &str| Field {
            name: "n".into(),
            field_type: t.into(),
            required: false,
        };
        assert_eq!(coerce(&f("number"), "3.5"), Ok(json!(3.5)));
        assert_eq!(coerce(&f("integer"), "42"), Ok(json!(42)));
        assert_eq!(coerce(&f("boolean"), "true"), Ok(json!(true)));
        assert_eq!(coerce(&f("string"), "42"), Ok(json!("42")));
        // An unknown type spec is not guessed at — the text as given,
        // for the registry's own validator to judge.
        assert_eq!(coerce(&f("date"), "2026-09-19"), Ok(json!("2026-09-19")));

        let err = coerce(&f("number"), "seven").expect_err("refused");
        assert!(err.contains("is not a number"), "{err}");
        let err = coerce(&f("number"), "NaN").expect_err("refused");
        assert!(
            err.contains("is not a number"),
            "a non-finite is not a number: {err}"
        );
        let err = coerce(&f("boolean"), "yes").expect_err("refused");
        assert!(err.contains("`true` or `false`"), "{err}");
    }

    /// An enum value is checked against the row's set, and the refusal
    /// names the set — the same contract `boss triage` holds its
    /// disposition to, generalised.
    #[test]
    fn an_enum_value_outside_the_rows_set_is_refused_naming_the_set() {
        let decision = Field {
            name: "decision".into(),
            field_type: "approved|changes-requested".into(),
            required: true,
        };
        assert_eq!(coerce(&decision, "approved"), Ok(json!("approved")));
        let err = coerce(&decision, "accepted").expect_err("refused");
        assert!(
            err.contains("not a value `decision` declares")
                && err.contains("approved, changes-requested"),
            "{err}"
        );
    }

    /// A structured field takes JSON, and a broken one names the file
    /// door rather than leaving the caller fighting shell quoting.
    #[test]
    fn a_structured_field_takes_json_and_a_broken_one_names_the_file_door() {
        let questions = Field {
            name: "questions".into(),
            field_type: "array".into(),
            required: true,
        };
        assert_eq!(
            coerce(&questions, r#"[{"anchor":"a"}]"#),
            Ok(json!([{ "anchor": "a" }]))
        );
        let err = coerce(&questions, "[{anchor}]").expect_err("refused");
        assert!(err.contains("--field-file questions=<PATH>"), "{err}");
        let err = coerce(&questions, r#"{"anchor":"a"}"#).expect_err("an object is not an array");
        assert!(err.contains("is not a JSON array"), "{err}");
    }

    /// HAZARD 1: the completion is the writes laid OVER the step's own
    /// metadata, so the procedure and the agent keys survive a PUT that
    /// replaces metadata wholesale.
    #[test]
    fn the_completion_keeps_the_steps_procedure_and_agent_keys() {
        let step = measure_step("ready");
        let writes = field_writes(
            &step,
            &given(&[
                ("controls_md", "seven links, three buttons"),
                ("needs_md", "in / working / out"),
                ("gaps_md", "1. no failure line on the queue read"),
            ]),
        )
        .expect("all three are declared");
        let body = completion(&step, &writes);
        assert_eq!(body["status"], "completed");
        assert_eq!(
            body["metadata"]["procedure"],
            "Read the PAGE and the DEPARTMENT, and write the difference.",
            "a wholesale metadata PUT would have deleted this"
        );
        assert_eq!(body["metadata"]["agent_profile"], "analyst");
        assert_eq!(body["metadata"]["human_only"], false);
        assert_eq!(
            body["metadata"]["gaps_md"],
            "1. no failure line on the queue read"
        );
        contract_check(&step, &body["metadata"]).expect("the row's contract is satisfied");
    }

    /// A completion short of a required field is refused HERE, in the
    /// registry's own words, rather than as a 422 the caller guesses
    /// against — the class `boss job file` was built to stop.
    #[test]
    fn a_missing_required_field_is_refused_before_the_round_trip() {
        let step = measure_step("ready");
        let writes =
            field_writes(&step, &given(&[("controls_md", "seven links")])).expect("declared");
        let body = completion(&step, &writes);
        let err = contract_check(&step, &body["metadata"]).expect_err("two are missing");
        assert!(
            err.contains("required field 'needs_md' is missing")
                && err.contains("required field 'gaps_md' is missing"),
            "names every missing field: {err}"
        );
        assert!(
            err.contains("--field-file <name>=<PATH>"),
            "names the door: {err}"
        );
    }

    /// Both flags read into one list: an argv value is trimmed, a
    /// file's bytes lose only the editor's final newline, and an
    /// emptied value is refused naming the quoting rule.
    #[test]
    fn the_two_flags_read_into_one_list_and_an_emptied_value_is_refused() {
        let dir = boss_testing::scratch::scratch_dir("boss-cli-step-field-file");
        let path = dir.join("gaps.md");
        boss_testing::scratch::write_file(&path, "1. the queue read has no failure line\n");
        let got = given_values(
            &["controls_md=  seven links ".to_string()],
            &[format!("gaps_md={}", path.display())],
        )
        .expect("both read");
        assert_eq!(
            got,
            given(&[
                ("controls_md", "seven links"),
                ("gaps_md", "1. the queue read has no failure line"),
            ])
        );
        let err = given_values(&["gaps_md=".to_string()], &[])
            .expect_err("an emptied value records nothing")
            .to_string();
        assert!(
            err.contains("SINGLE quotes") && err.contains("--field-file gaps_md="),
            "{err}"
        );
    }

    /// The whole verb against the stub: the PUT carries the merged
    /// metadata, the packet is read back, and the standing printed is
    /// the next step.
    #[tokio::test]
    async fn step_complete_merges_the_writes_and_confirms_by_reading_back() {
        let s = stub(vec![json!({
            "id": "0c4ff12b-1111-4000-8000-000000000001",
            "kind": "page-audit", "status": "open", "title": "Page audit: /it/queue",
            "metadata": { "route": "/it/queue", "department": "it" },
            "steps": [measure_step("ready"), step("file", "pending")],
        })])
        .await;
        let wire = Wire::at(s.base.clone(), named());
        complete(
            &wire,
            "0c4ff12b",
            "measure",
            &given(&[
                ("controls_md", "seven links, three buttons, four reads"),
                ("needs_md", "in / working / out"),
                ("gaps_md", "1. the queue read paints empty on failure"),
            ]),
        )
        .await
        .expect("completed");

        let puts = s.puts.lock().unwrap();
        assert_eq!(puts.len(), 1, "one PUT: {puts:?}");
        let (path, body) = &puts[0];
        assert!(
            path.ends_with("/33333333-3333-3333-3333-333333333333"),
            "{path}"
        );
        assert_eq!(body["status"], "completed");
        assert_eq!(body["metadata"]["agent_profile"], "analyst");
        assert_eq!(
            body["metadata"]["gaps_md"],
            "1. the queue read paints empty on failure"
        );
        drop(puts);
        // ONE PUT, from a step that is still open, is the whole
        // sequence — and the stored step is READ, not assumed. The
        // freeze in `update_step` is scoped to a step whose OLD status
        // is already terminal, so a `ready` step takes its metadata and
        // its completion in the same body (what `boss triage` and `boss
        // fold` have done since they landed). What a two-PUT sequence
        // would buy is a window where the fields are written and the
        // step is not completed — and if the completion were then
        // refused at done, a half-written open step to clean up by
        // hand.
        let after = s.packets.lock().unwrap();
        assert_eq!(after[0]["steps"][0]["status"], "completed");
        assert_eq!(
            after[0]["steps"][0]["metadata"]["procedure"],
            "Read the PAGE and the DEPARTMENT, and write the difference.",
            "the key that silently disappears: the STORED step still carries its procedure"
        );
        assert_eq!(after[0]["steps"][0]["metadata"]["agent_profile"], "analyst");
        assert_eq!(after[0]["steps"][0]["metadata"]["human_only"], false);
    }

    /// Every refusal happens BEFORE the write: an undeclared name, a
    /// missing required field, and a step that is not open each leave
    /// the stub with no PUT at all.
    #[tokio::test]
    async fn every_step_complete_refusal_happens_before_any_write() {
        let s = stub(vec![
            json!({
                "id": "0c4ff12b-1111-4000-8000-000000000001",
                "kind": "page-audit", "status": "open", "title": "Page audit: /it/queue",
                "metadata": {}, "steps": [measure_step("ready"), step("file", "pending")],
            }),
            json!({
                "id": "0c4ff12b-2222-4000-8000-000000000002",
                "kind": "page-audit", "status": "open", "title": "Page audit: /it/design",
                "metadata": {}, "steps": [measure_step("completed")],
            }),
        ])
        .await;
        let wire = Wire::at(s.base.clone(), named());

        let err = complete(
            &wire,
            "0c4ff12b-1111-4000-8000-000000000001",
            "measure",
            &given(&[("controls", "x")]),
        )
        .await
        .expect_err("undeclared");
        assert!(err.to_string().contains("no `controls` field"), "{err}");

        let err = complete(
            &wire,
            "0c4ff12b-1111-4000-8000-000000000001",
            "measure",
            &given(&[("controls_md", "x")]),
        )
        .await
        .expect_err("short of the contract");
        assert!(err.to_string().contains("'gaps_md' is missing"), "{err}");

        let err = complete(
            &wire,
            "0c4ff12b-2222-4000-8000-000000000002",
            "measure",
            &[],
        )
        .await
        .expect_err("already completed");
        assert!(err.to_string().contains("`measure` is completed"), "{err}");

        assert!(s.puts.lock().unwrap().is_empty(), "nothing was written");
    }

    /// A 204 that changed nothing is a failure, not a completion — the
    /// same read-back `boss job patch` insists on.
    #[tokio::test]
    async fn a_step_complete_the_packet_does_not_reflect_is_a_failure() {
        let s = stub(vec![json!({
            "id": "0c4ff12b-1111-4000-8000-000000000001",
            "kind": "page-audit", "status": "open", "title": "Page audit: /it/queue",
            "metadata": {}, "steps": [measure_step("ready")],
        })])
        .await;
        s.drop_puts.store(true, std::sync::atomic::Ordering::SeqCst);
        let wire = Wire::at(s.base.clone(), named());
        let err = complete(
            &wire,
            "0c4ff12b",
            "measure",
            &given(&[("controls_md", "a"), ("needs_md", "b"), ("gaps_md", "c")]),
        )
        .await
        .expect_err("the packet does not hold it");
        assert!(err.to_string().contains("reads back as ready"), "{err}");
    }

    // ------------------------------------------------------------------
    // `boss step release` — the door for a step a dead executor left
    // claimed (backlog e871febf).
    // ------------------------------------------------------------------

    const HELD_RUN: &str = "18c1a43e-5fac-43c3-a482-d701fef18c65";
    const HELD_JOB: &str = "bd93d2be-8de3-4033-a81d-fef50b819b37";
    const HELD_STEP: &str = "7df63c47-b2ae-4060-bcad-7700739989fa";

    /// A claimed build step in the shape the live board holds one
    /// (packet bd93d2be, read 2026-09-22): active, held by
    /// `agent-claude`, carrying the agent block — with or without the
    /// run edge, which is the split this packet is about.
    fn claimed(status: &str, edge: Option<&str>) -> Value {
        let mut md = Map::new();
        md.insert("agent_model".into(), json!("opus-5[1m]"));
        md.insert("agent_profile".into(), json!("builder"));
        md.insert("authority_role".into(), json!("platform-admin"));
        if let Some(run) = edge {
            md.insert(boss_jobs::agent_runs::EDGE_KEY.into(), json!(run));
        }
        json!({
            "id": HELD_STEP, "spec_slug": "build", "kind": "task", "status": status,
            "assignee_id": "agent-claude", "metadata": Value::Object(md),
        })
    }

    fn held_packet(step: Value) -> Value {
        json!({
            "id": HELD_JOB, "kind": "backlog-item", "status": "open",
            "title": "No evidence the core is stabilising",
            "metadata": {},
            "steps": [
                json!({ "id": "s-triage", "spec_slug": "triage", "status": "completed", "metadata": {} }),
                step,
                json!({ "id": "s-closed", "spec_slug": "closed", "status": "pending", "metadata": {} }),
            ],
        })
    }

    /// ONLY A CLAIMED STEP IS RELEASABLE. A `ready` step is already
    /// where a release puts one; writing the evidence anyway would
    /// record a takeover of work nobody held.
    #[test]
    fn releasable_takes_the_claimed_step_and_refuses_every_other_standing() {
        let held = held_packet(claimed("active", Some(HELD_RUN)));
        assert_eq!(
            releasable(&held, "build").expect("a claimed step is releasable")["id"],
            json!(HELD_STEP)
        );
        let free = held_packet(claimed("ready", None));
        let err = releasable(&free, "build").expect_err("nobody holds a ready step");
        assert!(
            err.contains("is ready") && err.contains("nothing to release"),
            "the refusal says what it is instead of releasing it: {err}"
        );
        let err = releasable(&held, "measure").expect_err("no such step");
        assert!(
            err.contains("no `measure` step") && err.contains("now at"),
            "and an unknown slug names where the packet stands: {err}"
        );
    }

    /// THE CLEAR GOES THROUGH THE MERGE DOOR, AND IT HAS TO.
    /// `update_step` CARRIES `agent_run` FORWARD on omission
    /// (b91a2103, pinned by `the_run_edge_survives_a_metadata_put`),
    /// so a PUT whose metadata simply lacks the key leaves the dead
    /// run pinned to the step — a silent no-op. `PATCH
    /// .../steps/{id}/metadata` deletes it on an explicit null, and is
    /// the only door that can.
    #[test]
    fn the_release_patch_clears_the_edge_with_an_explicit_null_and_records_who_took_it() {
        let body = release_patch(
            &claimed("active", Some(HELD_RUN)),
            "the run died and left it claimed",
            Some("emp-david"),
            "2026-09-22T06:00:00Z",
        );
        assert_eq!(
            body[boss_jobs::agent_runs::EDGE_KEY],
            Value::Null,
            "an explicit null, not an omission — omission is carried forward: {body}"
        );
        assert_eq!(
            body[RELEASED_KEY]["why"],
            json!("the run died and left it claimed")
        );
        assert_eq!(body[RELEASED_KEY]["from_run"], json!(HELD_RUN));
        assert_eq!(body[RELEASED_KEY]["by"], json!("emp-david"));
        assert_eq!(body[RELEASED_KEY]["at"], json!("2026-09-22T06:00:00Z"));
        // A step with NO edge — the population this packet measured —
        // releases the same way, and the record says the edge was
        // missing rather than leaving the reader to wonder.
        let bare = release_patch(
            &claimed("active", None),
            "why",
            None,
            "2026-09-22T06:00:00Z",
        );
        assert_eq!(bare[RELEASED_KEY]["from_run"], Value::Null);
        assert_eq!(
            bare[boss_jobs::agent_runs::EDGE_KEY],
            Value::Null,
            "the null is unconditional: clearing an absent key is a no-op, and branching \
             on it would be a second reading of the same fact"
        );
    }

    /// THE STATUS WRITE CARRIES NO METADATA, so it cannot undo the
    /// clear the PATCH just made: the PUT replaces metadata wholesale
    /// and omitting it keeps the stored document, which by then has no
    /// edge for the carry-forward to find.
    #[test]
    fn the_status_write_restores_exactly_what_the_claim_took() {
        let body = release_status();
        assert_eq!(body["status"], json!("ready"));
        assert_eq!(body["assignee_id"], Value::Null);
        assert!(
            body.get("metadata").is_none(),
            "nothing but the two fields a claim changed: {body}"
        );
    }

    /// A 204 IS A CLAIM. The read-back checks all four facts, and each
    /// one is a way the release can half-happen.
    #[test]
    fn the_read_back_refuses_a_release_that_only_half_took() {
        let mut freed = claimed("ready", None);
        freed["assignee_id"] = Value::Null;
        freed["metadata"][RELEASED_KEY] = json!({ "why": "the run died" });
        confirm_released(&freed, "the run died").expect("all four facts hold");

        let mut still_claimed = freed.clone();
        still_claimed["status"] = json!("active");
        assert!(
            confirm_released(&still_claimed, "the run died")
                .unwrap_err()
                .contains("active"),
            "a step that reads back active was not released"
        );

        let mut still_assigned = freed.clone();
        still_assigned["assignee_id"] = json!("agent-claude");
        assert!(
            confirm_released(&still_assigned, "the run died")
                .unwrap_err()
                .contains("agent-claude"),
            "a ready step still assigned is not handed back to the station"
        );

        let mut pinned = freed.clone();
        pinned["metadata"][boss_jobs::agent_runs::EDGE_KEY] = json!(HELD_RUN);
        assert!(
            confirm_released(&pinned, "the run died")
                .unwrap_err()
                .contains(boss_jobs::agent_runs::EDGE_KEY),
            "an edge that survived the clear is the b91a2103 no-op, and it is named"
        );

        let mut unrecorded = freed.clone();
        unrecorded["metadata"][RELEASED_KEY] = json!({ "why": "something else" });
        assert!(
            confirm_released(&unrecorded, "the run died")
                .unwrap_err()
                .contains("something else"),
            "and the reason read back must be the reason that was sent"
        );
    }

    /// THE WHOLE VERB against the stub: one PATCH, one PUT, and the
    /// stored step read back free — the stub models the server's own
    /// carry-forward, so a PUT-only release fails this test exactly as
    /// it would fail live.
    #[tokio::test]
    async fn step_release_hands_a_stranded_step_back_to_the_station() {
        let s = stub(vec![held_packet(claimed("active", Some(HELD_RUN)))]).await;
        let wire = Wire::at(s.base.clone(), named());
        release_step(
            &wire,
            "bd93d2be",
            "build",
            "the run that claimed it died (e871febf)",
        )
        .await
        .expect("released");

        let puts = s.puts.lock().unwrap();
        assert_eq!(
            puts.len(),
            2,
            "one metadata PATCH, then one status PUT: {puts:?}"
        );
        assert!(
            puts[0].0.ends_with("/metadata"),
            "the clear goes first: {puts:?}"
        );
        assert_eq!(puts[0].1[boss_jobs::agent_runs::EDGE_KEY], Value::Null);
        assert!(puts[1].1.get("metadata").is_none(), "{puts:?}");
        drop(puts);

        let step = s.packets.lock().unwrap()[0]["steps"][1].clone();
        assert_eq!(step["status"], json!("ready"));
        assert_eq!(step["assignee_id"], Value::Null);
        assert!(
            step["metadata"]
                .get(boss_jobs::agent_runs::EDGE_KEY)
                .is_none(),
            "the dead run is no longer named: {step}"
        );
        assert_eq!(
            step["metadata"][RELEASED_KEY]["why"],
            json!("the run that claimed it died (e871febf)")
        );
        assert_eq!(
            step["metadata"]["agent_model"],
            json!("opus-5[1m]"),
            "and the agent block rides through, so the inbox hands it out again: {step}"
        );
    }

    /// REFUSED BEFORE ANY WRITE: a blank reason, and a step nobody
    /// holds. The reason is the whole artifact a release leaves.
    #[tokio::test]
    async fn a_blank_reason_and_an_unheld_step_are_refused_before_the_first_write() {
        let s = stub(vec![held_packet(claimed("ready", None))]).await;
        let wire = Wire::at(s.base.clone(), named());
        let err = release_step(&wire, "bd93d2be", "build", "   ")
            .await
            .expect_err("blank");
        assert!(err.to_string().contains("--why is blank"), "{err}");
        let err = release_step(&wire, "bd93d2be", "build", "a reason")
            .await
            .expect_err("nobody holds it");
        assert!(err.to_string().contains("nothing to release"), "{err}");
        assert!(s.puts.lock().unwrap().is_empty(), "nothing was written");
    }
}

#[cfg(test)]
mod kind_field_tests {
    use super::*;

    fn step_types_fixture() -> Value {
        json!({"data": [
            {"kind": "checklist", "fields": [
                {"name": "items", "field_type": "array", "required": true}
            ]},
            {"kind": "task", "fields": []}
        ]})
    }

    /// The defect, in one assertion: a checklist step whose row names
    /// only its own fields still accepts `items`, because the API
    /// requires it.
    #[test]
    fn a_checklist_accepts_the_items_its_kind_requires() {
        let step = json!({
            "id": "11111111-1111-1111-1111-111111111111",
            "spec_slug": "measure",
            "kind": "checklist",
            "status": "ready",
            "fields": [
                {"name": "commits_ahead", "field_type": "string", "required": true}
            ]
        });
        let from_kind = kind_fields(&step_types_fixture(), "checklist");
        assert_eq!(
            from_kind.len(),
            1,
            "the registry declares items: {from_kind:?}"
        );

        let given = vec![
            Given {
                name: "commits_ahead".into(),
                value: "455".into(),
            },
            Given {
                name: "items".into(),
                value: "[{\"label\":\"drift measured\",\"checked\":true}]".into(),
            },
        ];
        let writes = field_writes_against(&step, &given, from_kind)
            .expect("the kind's required field is accepted");
        assert_eq!(writes["commits_ahead"], json!("455"));
        assert!(
            writes["items"].is_array(),
            "an `array` field is parsed as JSON, not stored as text: {:?}",
            writes["items"]
        );
    }

    /// The refusal that must SURVIVE: a name neither the row nor the
    /// kind declares is still refused. Widening the contract to the
    /// union must not widen it to everything — that guard is why a
    /// typo does not answer 204 and record an annotation nobody reads.
    #[test]
    fn a_name_neither_declares_is_still_refused() {
        let step = json!({
            "id": "11111111-1111-1111-1111-111111111111",
            "spec_slug": "measure",
            "kind": "checklist",
            "status": "ready",
            "fields": [{"name": "commits_ahead", "field_type": "string", "required": true}]
        });
        let from_kind = kind_fields(&step_types_fixture(), "checklist");
        let given = vec![Given {
            name: "committs_ahead".into(),
            value: "455".into(),
        }];
        let err =
            field_writes_against(&step, &given, from_kind).expect_err("a typo is still refused");
        assert!(err.contains("committs_ahead"), "{err}");
    }

    /// The row WINS on a collision: it may narrow what the kind leaves
    /// loose, and the row is the more specific contract.
    #[test]
    fn the_row_wins_when_both_declare_a_name() {
        let row = vec![Field {
            name: "items".into(),
            field_type: "string".into(),
            required: false,
        }];
        let kind = vec![Field {
            name: "items".into(),
            field_type: "array".into(),
            required: true,
        }];
        let merged = with_kind_fields(row, kind);
        assert_eq!(merged.len(), 1, "no duplicate entry: {merged:?}");
        assert_eq!(merged[0].field_type, "string", "the row's type stands");
    }

    /// A kind the registry does not know contributes nothing, rather
    /// than the verb inventing a contract one layer above the judge.
    #[test]
    fn an_unknown_kind_contributes_nothing() {
        assert!(kind_fields(&step_types_fixture(), "no-such-kind").is_empty());
        assert!(kind_fields(&json!({}), "checklist").is_empty());
    }
}

#[cfg(test)]
mod self_closing_tests {
    use super::*;

    fn alarm(finding: Option<&str>) -> Value {
        let mut md = serde_json::Map::new();
        if let Some(f) = finding {
            md.insert("estate_finding".into(), json!(f));
        }
        json!({ "id": "e1fea3b2-2ef7-4600-b96e-2214764373ba", "metadata": Value::Object(md) })
    }

    /// THE INCIDENT: routing a self-closing alarm to `build` took the
    /// step `estate.recover` completes, and the alarm outlived its
    /// condition by hours with nothing saying why.
    #[test]
    fn routing_a_self_closing_alarm_elsewhere_warns_and_names_what_it_takes_over() {
        let w = self_closing_warning(&alarm(Some("disk_tight:w-1")), "build")
            .expect("a self-closing alarm routed elsewhere is warned about");
        assert!(
            w.contains("disk_tight:w-1"),
            "the warning names the finding, so the reader knows which alarm: {w}"
        );
        assert!(
            w.contains("estate.recover"),
            "and the mechanism being taken over: {w}"
        );
        assert!(
            w.contains("stale"),
            "and the disposition that mechanism uses, which is what makes it checkable: {w}"
        );
        assert!(
            w.contains("If the finding is live"),
            "and says when routing it IS right, so the warning is not read as a refusal: {w}"
        );
    }

    /// THE CONTROLS, and there are two because the warning must not
    /// fire on ordinary triage — which is nearly every triage there is.
    /// A warning on every packet is noise, and noise becomes unread,
    /// which is the class this whole packet belongs to.
    #[test]
    fn an_ordinary_packet_is_not_warned_about() {
        assert_eq!(
            self_closing_warning(&alarm(None), "build"),
            None,
            "a packet carrying no estate_finding is not an alarm and is not the \
             recovery's to close"
        );
        assert_eq!(
            self_closing_warning(&json!({ "id": "x" }), "build"),
            None,
            "nor is one with no metadata at all"
        );
    }

    /// AND THE DISPOSITION THE RECOVERY ITSELF USES IS NOT A TAKEOVER.
    /// Reaching the same terminal by hand is the same outcome; warning
    /// there would be telling an operator off for agreeing.
    #[test]
    fn closing_it_stale_by_hand_is_not_taking_anything_over() {
        assert_eq!(
            self_closing_warning(&alarm(Some("disk_tight:w-1")), "stale"),
            None,
            "`stale` is the disposition estate.recover writes — a human writing it \
             reaches the same terminal, and nothing is lost"
        );
        assert!(
            self_closing_warning(&alarm(Some("disk_tight:w-1")), "duplicate").is_some(),
            "but any OTHER disposition skips the withdrawal terminals the recovery needs"
        );
    }
}

#[cfg(test)]
mod design_link_tests {
    use super::*;

    fn design(id: &str, answers: Option<&str>) -> Value {
        let mut md = serde_json::Map::new();
        if let Some(a) = answers {
            md.insert("answers".into(), json!(a));
        }
        json!({ "id": id, "metadata": Value::Object(md) })
    }

    const PACKET: &str = "6c2eba00-1111-4111-8111-111111111111";
    const OTHER: &str = "dd6d44b7-2222-4222-8222-222222222222";
    const DESIGN: &str = "f2cdff23-3333-4333-8333-333333333333";

    /// THE INCIDENT (2026-09-20): a design linked only by the step
    /// field. The rule follows the design's OWN edge, so it never
    /// fires, and the packet waits forever while the design leaves the
    /// reviewer's queue. David saw exactly that and reasonably
    /// concluded his review had been lost.
    #[test]
    fn a_design_carrying_no_answers_edge_is_refused_with_the_door_named() {
        let err = design_link_check(PACKET, &design(DESIGN, None))
            .expect_err("a design that names nobody cannot close this packet");
        assert!(
            err.contains("NOTHING follows"),
            "the refusal must say the link would be one NOTHING follows: {err}"
        );
        assert!(
            err.contains("boss design"),
            "and name the door that writes both representations: {err}"
        );
        assert!(
            err.contains("2c7dd4ab"),
            "and cite the measurement, so the next reader can check it: {err}"
        );
    }

    /// The other half of wrong: a design that answers a DIFFERENT
    /// packet. Linking it here would record a second, contradicting
    /// claim about what the design decides — and the other packet is
    /// the one that would close.
    #[test]
    fn a_design_answering_another_packet_is_refused_and_names_it() {
        let err = design_link_check(PACKET, &design(DESIGN, Some(OTHER)))
            .expect_err("a design pointed elsewhere cannot close this packet");
        assert!(
            err.contains(&OTHER[..8]),
            "the refusal names the packet the design actually answers: {err}"
        );
        assert!(
            err.contains(&PACKET[..8]),
            "and the one that would have waited forever: {err}"
        );
    }

    /// THE CONTROL. The correct pairing passes — without it the two
    /// refusals above would be satisfied by a check that refuses
    /// everything, which would make `boss design --answers` unusable.
    #[test]
    fn the_matching_pair_is_accepted() {
        design_link_check(PACKET, &design(DESIGN, Some(PACKET)))
            .expect("a design that answers this packet links fine");
    }
}
