//! `boss correct` — append a correction beside a completed step
//! (design 4105b020, backlog 56727f95).
//!
//! WHY. A completed step is a fact, and the step API rightly refuses to
//! rewrite it. On 2026-09-19 two such facts were wrong — prose damaged
//! by shell substitution, "Ordering trap confirmed:  is required of
//! every rule" on f3e091f0 and "spec_slug , title Settled" on ccac28a1
//! — and the only route was a job-metadata key of the author's own
//! invention, which no reader of the step ever saw. The door is now
//! `POST /api/jobs/{id}/steps/{step_id}/corrections`: it appends one
//! entry to the job's reserved `corrections` list, and the job GET
//! hands each step its own entries, so every reader shows them beside
//! the original rather than depending on the next reader happening to
//! open the job's metadata. This verb is that door from a terminal.
//!
//! THE -FILE TWINS ARE THE POINT, not a convenience. The prose this
//! verb carries is, by definition, prose that already died once in
//! argv (backlog 2376b89e): `--reads` quotes the damaged text and
//! `--should-read` says what it should have been, and a backticked
//! word in either, inside double quotes, is run by the shell and lands
//! as the same hole. So each has a `-file` twin through `prose`, where
//! no word expansion happens at all.
//!
//! The SERVER judges: an open step, a field the step does not hold and
//! an excerpt that is not in the stored text are its refusals, and this
//! verb does not keep a second copy of them. It resolves the step by
//! slug (refusing naming the slugs the packet has), posts, and reads the
//! packet back — a 201 is a claim; the entry on the step is the fact.
//!
//! Signed as the actor running the verb (`identity`): the entry's `by`
//! is who corrected, never the conductor.

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};

use crate::steps::Wire;

#[derive(clap::Subcommand)]
pub enum Cmd {
    /// Append a correction beside a completed step: the step stays as it was, and every reader is handed the correction.
    ///
    /// Quote the damaged text EXACTLY as the step stores it (`--reads`)
    /// and say what it should read (`--should-read`); the API refuses an
    /// open step, a field the step does not hold, and an excerpt that is
    /// not in the stored text. `--withdraws <index>` withdraws an earlier
    /// correction of the same step, by appending, with `--why`.
    ///
    /// SINGLE-quote every prose value, or use the `-file` twins: inside
    /// double quotes a backticked word is run by the shell and lands as
    /// a hole (backlog 2376b89e) — the very damage being corrected.
    Correct {
        /// The packet: the full uuid, or 8+ characters of it (open or closed).
        packet: String,
        /// The corrected step's slug, as the Workflow row titles it (`triage`, `reported`).
        #[arg(long)]
        step: String,
        /// The step metadata field being corrected (`evidence`, `summary`).
        #[arg(long, required_unless_present = "withdraws")]
        field: Option<String>,
        /// The damaged text, quoted exactly as the step stores it.
        #[arg(long, conflicts_with = "reads_file")]
        reads: Option<String>,
        /// The damaged text, read from this file. Exclusive with --reads.
        #[arg(long)]
        reads_file: Option<std::path::PathBuf>,
        /// What the quoted text should read.
        #[arg(long, conflicts_with = "should_read_file")]
        should_read: Option<String>,
        /// What it should read, from this file. Exclusive with --should-read.
        #[arg(long)]
        should_read_file: Option<std::path::PathBuf>,
        /// Why — the cause of the damage, or why a correction is withdrawn.
        #[arg(long, conflicts_with = "why_file")]
        why: Option<String>,
        /// Why, from this file. Exclusive with --why.
        #[arg(long)]
        why_file: Option<std::path::PathBuf>,
        /// Withdraw the correction at this index (it must correct the same step).
        #[arg(long, conflicts_with_all = ["field", "reads", "reads_file", "should_read", "should_read_file"])]
        withdraws: Option<usize>,
    },
}

pub async fn dispatch(cmd: Cmd) -> Result<()> {
    let Cmd::Correct {
        packet,
        step,
        field,
        reads,
        reads_file,
        should_read,
        should_read_file,
        why,
        why_file,
        withdraws,
    } = cmd;
    let why = crate::prose::opt_text_or_file("--why", "--why-file", why, why_file.as_deref())?;
    let body = match withdraws {
        Some(index) => withdrawal_body(index, why.as_deref())?,
        None => correction_body(
            field.as_deref().unwrap_or_default(),
            &crate::prose::text_or_file("--reads", "--reads-file", reads, reads_file.as_deref())?,
            &crate::prose::text_or_file(
                "--should-read",
                "--should-read-file",
                should_read,
                should_read_file.as_deref(),
            )?,
            why.as_deref(),
        ),
    };
    let wire = Wire::live()?;
    println!("{}", correct(&wire, &packet, &step, body).await?);
    Ok(())
}

// ----------------------------------------------------------------------
// The pure core.
// ----------------------------------------------------------------------

/// The door's body for a correction.
pub(crate) fn correction_body(
    field: &str,
    reads: &str,
    should_read: &str,
    why: Option<&str>,
) -> Value {
    json!({
        "field": field,
        "reads": reads,
        "should_read": should_read,
        "why": why.unwrap_or(""),
    })
}

/// The door's body for a withdrawal — refused here without a reason,
/// because a withdrawal that says nothing is the key-name-only join
/// (gate-run 9a2576fb) the list exists to retire.
pub(crate) fn withdrawal_body(index: usize, why: Option<&str>) -> Result<Value> {
    match why.map(str::trim).filter(|w| !w.is_empty()) {
        Some(why) => Ok(json!({ "withdraws": index, "why": why })),
        None => bail!(
            "--withdraws needs --why (or --why-file): a withdrawal that does not say why \
             leaves the next reader two entries and no way to choose between them"
        ),
    }
}

/// The step a correction targets, by slug — ANY status: the server
/// decides whether it can be corrected, and says so. Refused here only
/// when the packet has no such step, naming the slugs it has.
pub(crate) fn step_by_slug<'a>(packet: &'a Value, slug: &str) -> Result<&'a Value, String> {
    let steps = crate::envelope::steps(packet);
    steps
        .iter()
        .find(|s| s.get("spec_slug").and_then(Value::as_str) == Some(slug))
        .copied()
        .ok_or_else(|| {
            let slugs: Vec<&str> = steps
                .iter()
                .filter_map(|s| s.get("spec_slug").and_then(Value::as_str))
                .collect();
            format!(
                "the packet has no `{slug}` step — its steps are: {}",
                if slugs.is_empty() {
                    "(none with a slug)".to_string()
                } else {
                    slugs.join(", ")
                }
            )
        })
}

/// The entry at `index` as the read-back hands it to the step — the
/// fact behind the door's 201.
pub(crate) fn handed_to_step(step: &Value, index: u64) -> Option<&Value> {
    step.get("corrections")
        .and_then(Value::as_array)?
        .iter()
        .find(|c| c.get("index").and_then(Value::as_u64) == Some(index))
}

/// Resolve, post, read back, and say what the record now holds.
pub(crate) async fn correct(wire: &Wire, packet: &str, slug: &str, body: Value) -> Result<String> {
    // The server resolves an 8+ character prefix on every door
    // (cd7b0054), open or closed — and a correction is usually of a
    // closed packet, which an open-only resolve would miss.
    let job = wire
        .call(reqwest::Method::GET, &format!("/api/jobs/{packet}"), None)
        .await?
        .with_context(|| format!("packet {packet} read back empty"))?;
    let job_id = crate::envelope::job_id(&job)
        .context("the packet has no id")?
        .to_string();
    let step = step_by_slug(&job, slug).map_err(|e| anyhow!("{}: {e}", short(&job_id)))?;
    let step_id = step
        .get("id")
        .and_then(Value::as_str)
        .context("the step carries no id")?
        .to_string();

    let answer = wire
        .call(
            reqwest::Method::POST,
            &format!("/api/jobs/{job_id}/steps/{step_id}/corrections"),
            Some(body),
        )
        .await?
        .context("the corrections door answered with no body")?;
    let index = answer
        .get("index")
        .and_then(Value::as_u64)
        .context("the corrections door answered without an index")?;

    let after = wire
        .call(reqwest::Method::GET, &format!("/api/jobs/{job_id}"), None)
        .await?
        .context("the packet read back empty")?;
    let step_after = step_by_slug(&after, slug).map_err(|e| anyhow!("{e}"))?;
    let entry = handed_to_step(step_after, index).ok_or_else(|| {
        anyhow!(
            "the door answered index {index}, but the packet read back does not hand that \
             correction to `{slug}` — not recorded as a correction a reader will see"
        )
    })?;
    let what = match entry.get("withdraws").and_then(Value::as_u64) {
        Some(n) => format!("withdraws correction [{n}]"),
        None => format!(
            "`{}` reads {:?} → should read {:?}",
            entry.get("field").and_then(Value::as_str).unwrap_or("?"),
            entry.get("reads").and_then(Value::as_str).unwrap_or(""),
            entry
                .get("should_read")
                .and_then(Value::as_str)
                .unwrap_or("")
        ),
    };
    Ok(format!(
        "boss correct: {} \"{}\" — correction [{index}] on `{slug}` recorded by {}: {what}\n  \
         the step is unchanged; its readers are handed the correction beside it",
        short(&job_id),
        crate::envelope::job_title(&after).unwrap_or("?"),
        entry.get("by").and_then(Value::as_str).unwrap_or("?"),
    ))
}

fn short(id: &str) -> String {
    crate::train::id8(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_correction_body_carries_the_four_keys_the_door_reads() {
        let b = correction_body(
            "evidence",
            "confirmed:  is",
            "confirmed: `why` is",
            Some("ate it"),
        );
        assert_eq!(
            b,
            json!({ "field": "evidence", "reads": "confirmed:  is",
                    "should_read": "confirmed: `why` is", "why": "ate it" })
        );
        assert_eq!(correction_body("f", "r", "s", None)["why"], "");
    }

    #[test]
    fn a_withdrawal_without_a_reason_is_refused_before_the_round_trip() {
        assert!(withdrawal_body(0, None).is_err());
        assert!(withdrawal_body(0, Some("  ")).is_err());
        assert_eq!(
            withdrawal_body(2, Some("wrong")).unwrap(),
            json!({ "withdraws": 2, "why": "wrong" })
        );
    }

    #[test]
    fn an_unknown_slug_is_refused_naming_the_slugs_the_packet_has() {
        let p = json!({ "steps": [
            { "id": "a", "spec_slug": "triage", "status": "completed" },
            { "id": "b", "spec_slug": "build", "status": "active" },
        ]});
        assert_eq!(step_by_slug(&p, "build").unwrap()["id"], "b");
        let e = step_by_slug(&p, "reported").unwrap_err();
        assert!(
            e.contains("`reported`") && e.contains("triage, build"),
            "{e}"
        );
    }

    // ------------------------------------------------------------------
    // The whole verb against the REAL jobs router (in-memory adapter) on
    // an ephemeral port — the same handler, refusals and read-back that
    // production serves, driven with a named caller as a value.
    // ------------------------------------------------------------------

    use std::sync::Arc;

    use boss_core::job::{Job, JobId, JobStatus, Priority, Step, StepStatus, Subject};
    use boss_jobs::JobsRepository;
    use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};

    const DAMAGED: &str = "Ordering trap confirmed:  is required of every rule";

    async fn serve() -> (String, Arc<boss_jobs::InMemoryJobs>, Job) {
        let jobs = Arc::new(boss_jobs::InMemoryJobs::new());
        let job = Job {
            id: JobId::from_uuid(
                uuid::Uuid::parse_str("f3e091f0-0000-4000-8000-000000000001").unwrap(),
            ),
            kind: "backlog-item".into(),
            workflow_version: 1,
            subject: Subject::new("custom", "/it/backlog"),
            title: "Ordering trap".into(),
            owner_id: "emp-1".into(),
            status: JobStatus::Closed,
            priority: Priority::Standard,
            opened_on: chrono::NaiveDate::from_ymd_opt(2026, 9, 19).unwrap(),
            opened_at: None,
            due_on: None,
            closed_on: None,
            metadata: json!({}),
            tags: vec![],
            partition: boss_core::partition::Partition::Real,
        };
        jobs.create_job(&job).await.unwrap();
        let mut triage = Step::new(job.id, "task", "Measure the claim, choose a route", 0);
        triage.spec_slug = Some("triage".into());
        triage.status = StepStatus::Completed;
        triage.metadata = json!({ "evidence": DAMAGED });
        jobs.add_step(&triage).await.unwrap();

        let bus = boss_testing::RecordingEventBus::new();
        let bus_dyn: Arc<dyn boss_core::port::EventBus> = bus.clone();
        let publisher = boss_core::publisher::DomainPublisher::new(bus_dyn, "jobs");
        let policy: Arc<dyn PolicyClient> = Arc::new(
            FakePolicyClient::builder()
                .allow(
                    "platform-admin",
                    Action::Update,
                    Resource::job(),
                    Scope::All,
                )
                // The verb reads the packet back through the job GET,
                // scoped by policy Read on job since backlog 046832d3.
                .allow("platform-admin", Action::Read, Resource::job(), Scope::All)
                .build(),
        );
        let app = boss_jobs::http::router(boss_jobs::http::JobsApiState::minimal(
            jobs.clone(),
            bus,
            publisher,
            policy,
            Arc::new(boss_clock_client::WallClockClient),
        ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (format!("http://{addr}"), jobs, job)
    }

    fn named() -> Option<crate::identity::Caller> {
        Some(crate::identity::Caller {
            id: "claude@algedonic.dev".into(),
            source: crate::identity::Source::Env,
        })
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_verb_records_a_correction_a_reader_of_the_step_is_handed() {
        let (base, jobs, job) = serve().await;
        let wire = Wire::at(base, named());
        // By its 8-character prefix, on a CLOSED packet.
        let out = correct(
            &wire,
            "f3e091f0",
            "triage",
            correction_body(
                "evidence",
                "confirmed:  is required",
                "confirmed: `why` is required",
                Some("the backticked word was run by the shell (2376b89e)"),
            ),
        )
        .await
        .expect("recorded");
        assert!(out.contains("correction [0] on `triage`"), "{out}");
        assert!(
            out.contains("claude@algedonic.dev"),
            "signed by the runner: {out}"
        );

        let stored = jobs.get_job(&job.id).await.unwrap().unwrap();
        assert_eq!(stored.metadata["corrections"][0]["field"], "evidence");
        assert_eq!(
            stored.metadata["corrections"][0]["by"],
            "claude@algedonic.dev"
        );

        let out = correct(
            &wire,
            "f3e091f0",
            "triage",
            withdrawal_body(0, Some("no")).unwrap(),
        )
        .await
        .expect("withdrawn");
        assert!(
            out.contains("[1]") && out.contains("withdraws correction [0]"),
            "{out}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_servers_refusal_reaches_the_terminal_with_its_reason() {
        let (base, jobs, job) = serve().await;
        let wire = Wire::at(base, named());
        let err = correct(
            &wire,
            "f3e091f0",
            "triage",
            correction_body("evidence", "confirmed: `why` is", "x", None),
        )
        .await
        .expect_err("the excerpt is not what is stored");
        assert!(err.to_string().contains("does not appear"), "{err}");
        let err = correct(
            &wire,
            "f3e091f0",
            "reported",
            correction_body("a", "b", "c", None),
        )
        .await
        .expect_err("no such step");
        assert!(err.to_string().contains("triage"), "{err}");
        assert!(
            jobs.get_job(&job.id)
                .await
                .unwrap()
                .unwrap()
                .metadata
                .get("corrections")
                .is_none(),
            "nothing was recorded"
        );
    }
}
