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
        } => {
            let change = crate::prose::text_or_file(
                "--change",
                "--change-file",
                change,
                change_file.as_deref(),
            )?;
            fold(&wire, &design, &change).await
        }
        Cmd::Hold { car, reason } => hold(&wire, &car, Some(&reason)).await,
        Cmd::Release { car } => hold(&wire, &car, None).await,
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

/// What `boss fold` writes: `fold_change`, if the row declares it.
pub(crate) fn fold_writes(step: &Value, change: &str) -> Result<Map<String, Value>, String> {
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

pub(crate) async fn fold(wire: &Wire, design: &str, change: &str) -> Result<()> {
    let packet = wire.resolve(design, Some("design-doc")).await?;
    let step = foldable(&packet).map_err(|e| anyhow!("{e}"))?;
    let writes = fold_writes(step, change).map_err(|e| anyhow!("{}: {e}", short(&packet)))?;
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
        let writes = fold_writes(step, " docs/architecture-decisions.md gains a section ").unwrap();
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
        let err = fold_writes(step, " ").expect_err("blank");
        assert!(err.contains("--change is blank"), "{err}");
        let bare = json!({ "spec_slug": "fold", "status": "ready", "fields": [], "metadata": {} });
        let err = fold_writes(&bare, "x").expect_err("no fold_change");
        assert!(err.contains("no `fold_change` field"), "{err}");
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
                if let Some(obj) = sent.as_object() {
                    for (k, v) in obj {
                        step[k] = v.clone();
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
        let err = fold(&wire, "0d2e1655", "x").await.expect_err("review open");
        assert!(
            err.to_string().contains("order, delete-bare-metal"),
            "{err}"
        );
        assert!(s.puts.lock().unwrap().is_empty());
        // Not a design-doc at all: the kind filter finds nothing.
        let s = stub(vec![packet("backlog-item", vec![step("triage", "ready")])]).await;
        let wire = Wire::at(s.base.clone(), named());
        let err = fold(&wire, "0d2e1655", "x")
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
}
