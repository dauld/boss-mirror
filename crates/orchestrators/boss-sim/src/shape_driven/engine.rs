//! `simulate_day` — the day-loop primitive for the shape-driven
//! sim. Composes tenant config + Workflow registry + StepRegistry +
//! Subject pool + RNG into a list of "things that happened today"
//! plus state mutations.
//!
//! Stages 2 + 3: Job creation, eager Step materialization at
//! Job-open time, daily Step advancement (Ready → Completed with
//! metadata filled by the faker), and Job closure when every Step is
//! terminal. The sim is a Workflow-v2 executor client: it posts
//! job-create + step-completions to the live boss-jobs-api and the
//! SERVER owns step re-evaluation (Pending→Ready / Pending→Skipped /
//! terminal→close); the sim runs the shared `reevaluate` locally only
//! to keep its in-process projection consistent for the next tick.
//! Stages 4 + 5 (Subject birth + anomaly injection +
//! on_complete_create runtime hook) ship in follow-up commits.

use boss_core::job::{Job, JobId, JobStatus, Priority, Subject};
use boss_core::primitives::Subject as SubjectTrait;
use boss_jobs::registry::WorkflowSpec;
use boss_jobs::step_registry::StepRegistry;

use crate::engines::{SimEventBus, Tick};
use crate::output::SimOutput;
use crate::rng::{Rng, poisson_sample};
use crate::shape_driven::sampler::count_jobs_for_tick;
use crate::shape_driven::state::{DaySummary, ShapeDrivenState};
use crate::shape_driven::tenant::{SubjectRate, TenantConfig};

/// Bus source identifier published on every event the HumanWorker
/// engine emits. CounterpartyEngine listens for events whose `source`
/// starts with this prefix to filter Boss-initiated triggers from
/// other engines' emissions.
pub const HUMAN_WORKER_SOURCE: &str = "human-worker";

/// Advance the sim by one day. Reads tenant rates against the
/// Workflow registry, samples per-kind Job counts, picks a Subject
/// for each new Job from the state's pool, and inserts the Job
/// into state.
///
/// Returns a `DaySummary` capturing what happened — useful for
/// per-day SSE streaming and for tests that want to assert on
/// rates without crawling state directly.
///
/// Skips:
/// - Days outside the tenant's operating_days list.
/// - Workflows whose subject_kinds is empty (degenerate — the
///   author left them off).
/// - Workflows whose first subject_kind has no available Subjects
///   in the pool. The summary records the skip in
///   `jobs_skipped_no_subject` so the test surfaces missing
///   seeds.
/// - Workflows whose slug doesn't match any spec in `workflows`.
///   Counts go to `jobs_skipped_unknown_kind`.
#[allow(clippy::too_many_arguments)]
pub fn simulate_day(
    workflows: &[WorkflowSpec],
    step_registry: &StepRegistry,
    tenant: &TenantConfig,
    day: chrono::NaiveDate,
    state: &mut ShapeDrivenState,
    rng: &mut Rng,
    output: &mut dyn SimOutput,
    bus: &mut SimEventBus,
) -> DaySummary {
    simulate_tick_with_handlers(
        workflows,
        step_registry,
        tenant,
        day,
        &Tick::day(),
        state,
        rng,
        output,
        bus,
    )
}

/// Tick-aware day loop — Phase A of the sub-day-tick rollout
/// (TODO.md "Hourly tick granularity for the live sim").
///
/// Per-tick math scales by `tick.day_fraction()`:
/// - **Poisson draws** scale lambda → per-day expected volume invariant
///   (24×1h ticks fire ~the same number of Jobs as 1×1d).
/// - **Step-completion rolls** scale exponentially → per-day completion
///   probability invariant (see `engines::tick::Tick`).
/// - **Subject-birth Poisson** scales the same way.
///
/// Day-anchored mechanisms (cadence rows, shock-rolls,
/// `days_simulated` counter) gate on `tick.is_first_in_day` so they
/// fire once per sim-day rather than once per tick. `Tick::day()`
/// always sets that to true so the legacy day-tick path lands
/// bit-for-bit equivalent.
#[allow(clippy::too_many_arguments)]
pub fn simulate_tick_with_handlers(
    workflows: &[WorkflowSpec],
    step_registry: &StepRegistry,
    tenant: &TenantConfig,
    day: chrono::NaiveDate,
    tick: &Tick,
    state: &mut ShapeDrivenState,
    rng: &mut Rng,
    output: &mut dyn SimOutput,
    bus: &mut SimEventBus,
) -> DaySummary {
    let mut summary = DaySummary {
        day: Some(day),
        ..Default::default()
    };

    if !tenant.meta.is_operating_day(day) {
        if tick.is_first_in_day {
            state.counters.days_simulated += 1;
        }
        return summary;
    }

    // Phase B-2 — operating-hours filter. When `[meta.operating_hours]`
    // is set + this tick's window falls outside the day's open
    // window (people aren't working), skip the HumanWorker layer
    // for this tick. Calendar-anchored mechanisms (shocks fire on
    // quarter start; days_simulated still increments) keep
    // running; only Job creation + step advancement gate on the
    // window.
    //
    // At day-tick (`tick_duration = "1d"`) this is a no-op: the
    // day-tick covers `[0, 24)` so any non-empty window overlaps.
    let in_hours = tenant.meta.is_operating_at_tick(day, tick);
    if !in_hours {
        if tick.is_first_in_day {
            state.counters.days_simulated += 1;
        }
        return summary;
    }

    // 0. Roll new shocks at quarter starts + prune expired ones.
    //    Quarter-anchored — fire once per sim-day, not per tick.
    if tick.is_first_in_day {
        roll_and_prune_shocks(tenant, day, state, rng);
    }

    // 2. Birth new Subjects from tenant.subject_rates. Per-tick
    //    Poisson lambda scales by tick.day_fraction(); per-day
    //    expected birth count is invariant.
    birth_subjects(tenant, day, tick, state, rng, output, &mut summary);

    // 3. Sample new Jobs from the Workflow registry's tenant rates.
    //    The sampler's weekday/weekend/holiday demand multipliers read
    //    the `us-banking` calendar DATA carried on state (seeded by the
    //    daemon from boss-calendar; the `Default` carries the 2026
    //    holiday set for tests). Clone once per tick into a local so
    //    the immutable calendar borrow doesn't conflict with the
    //    mutable `state` borrows in the loop below.
    let us_banking = state.us_banking.clone();
    let mut kinds: Vec<(&String, &crate::shape_driven::tenant::JobRate)> =
        tenant.job_rates.iter().collect();
    kinds.sort_by(|a, b| a.0.cmp(b.0));

    for (kind, rate) in kinds {
        let Some(spec) = workflows.iter().find(|jk| &jk.kind == kind) else {
            // Unknown kind: bookkeep the Poisson draw the same way
            // we did before the cadence path was added. Tick-scaled
            // so an unknown kind doesn't 24x its drop count at
            // hourly granularity.
            let count = count_jobs_for_tick(rate, day, tick, &us_banking, rng) as u64;
            summary.jobs_skipped_unknown_kind += count;
            state.counters.jobs_skipped_unknown_kind += count;
            continue;
        };
        if spec.subject_kinds.is_empty() {
            let count = count_jobs_for_tick(rate, day, tick, &us_banking, rng) as u64;
            summary.jobs_skipped_no_subject += count;
            state.counters.jobs_skipped_no_subject += count;
            continue;
        }
        let subject_kind = &spec.subject_kinds[0];

        // 3a. Deterministic per-Subject cadence — "Sierra Networks
        //     orders every Tuesday at 09:00." Phase B-2: cadence
        //     rows now anchor on (weekday, time_of_day). The
        //     `fires_on_tick` check returns true when both the
        //     weekday matches AND the tick's window
        //     `[start_hour, start_hour + duration)` covers the
        //     configured `time_of_day` (defaults to midnight
        //     for legacy rows, so day-tick callers see no
        //     behavior change).
        for cadence in &rate.subject_cadence {
            if !cadence.fires_on_tick(day, tick) {
                continue;
            }
            create_job_with_steps(
                spec,
                subject_kind,
                &cadence.subject_id,
                day,
                state,
                &mut summary,
                kind,
                tenant,
                rng,
                output,
                bus,
                step_registry,
            );
        }

        // 3b. Poisson draw for ad-hoc volume. Active shocks scaling
        //     this kind (without a subject filter) lift/dampen the
        //     whole rate; subject-targeted shocks only kick in
        //     after `pick_subject` returns. Per-tick lambda is
        //     `effective_rate × kind_wide_mult × tick.day_fraction`.
        let kind_wide_mult = active_shock_multiplier(state, kind, None);
        let count =
            count_jobs_with_multiplier(rate, day, tick, &us_banking, rng, kind_wide_mult) as u64;
        if count == 0 {
            continue;
        }
        for _ in 0..count {
            let Some(subject_id) =
                pick_subject(state, subject_kind, &rate.subject_distribution, rng)
            else {
                summary.jobs_skipped_no_subject += 1;
                state.counters.jobs_skipped_no_subject += 1;
                continue;
            };
            // Subject-targeted shocks: roll once per draw. With
            // multiplier 2.0 we keep the original draw + add a
            // second one with probability 1.0; with 0.5 we flip a
            // coin to drop the draw. Falls through unchanged for
            // multiplier 1.0 (no shock match).
            let subj_mult = active_shock_multiplier(state, kind, Some(&subject_id));
            let extra_draws = expand_shock_draws(subj_mult, rng);
            for _ in 0..extra_draws {
                create_job_with_steps(
                    spec,
                    subject_kind,
                    &subject_id,
                    day,
                    state,
                    &mut summary,
                    kind,
                    tenant,
                    rng,
                    output,
                    bus,
                    step_registry,
                );
            }
        }
    }

    if tick.is_first_in_day {
        state.counters.days_simulated += 1;
    }
    summary
}

/// Open one Job, materialize its Steps from the Workflow step_graph,
/// leave the earliest-eligible Steps Ready, roll any
/// per-Workflow anomaly probabilities, and insert everything into
/// state.
#[allow(clippy::too_many_arguments)] // engine internals; explicit > a context struct here
/// Roll a Priority for a new Job based on the Workflow. Mirrors a
/// realistic operating distribution rather than the prior
/// "everything is Standard" sim flatness. The mix is Workflow-aware
/// so e.g. `equipment-preventive-maintenance` skews toward Scheduled (planned preventive maintenance
/// visits), `morning-brew` skews Standard (it's the daily
/// production heartbeat), and ad-hoc gets a richer mix because
/// that's where the "something broke" Jobs land. Probabilities
/// roll once per Job from the seeded Rng so replay determinism
/// stays intact.
fn priority_for_kind(kind: &str, rng: &mut crate::rng::Rng) -> Priority {
    let r = rng.next();
    // (cum_emergency, cum_urgent, cum_standard) — the rest tail
    // into Scheduled.
    let (e, u, s) = match kind {
        "equipment-preventive-maintenance" => (0.005, 0.04, 0.30),
        "morning-brew" => (0.0, 0.05, 0.93),
        "wholesale-keg-order" => (0.005, 0.10, 0.92),
        "ingredient-restock" => (0.0, 0.20, 0.85),
        "seasonal-release" => (0.0, 0.05, 0.80),
        "tap-launch" => (0.0, 0.10, 0.85),
        "ad-hoc" => (0.05, 0.30, 0.85),
        _ => (0.005, 0.10, 0.85),
    };
    if r < e {
        Priority::Emergency
    } else if r < u {
        Priority::Urgent
    } else if r < s {
        Priority::Standard
    } else {
        Priority::Scheduled
    }
}

#[allow(clippy::too_many_arguments)]
fn create_job_with_steps(
    spec: &WorkflowSpec,
    subject_kind: &str,
    subject_id: &str,
    day: chrono::NaiveDate,
    state: &mut ShapeDrivenState,
    summary: &mut DaySummary,
    kind_slug: &str,
    tenant: &TenantConfig,
    rng: &mut Rng,
    output: &mut dyn SimOutput,
    bus: &mut SimEventBus,
    // Steps are materialized + open-time-evaluated by the v2 materializer
    // now; create no longer needs the registry. Kept in the signature
    // for call-site symmetry — drop in the sim-boundary cleanup.
    _step_registry: &StepRegistry,
) -> JobId {
    // Anomalies fire at Job-open time. Each `*_probability` in the
    // Workflow's anomaly table rolls once per Job — if hit, the
    // counters tick. This v0 doesn't yet translate fires into
    // structural state changes (cancel / delay / branch); that's
    // anomaly-handling Stage 6.
    if let Some(anomalies) = tenant.anomalies.get(kind_slug) {
        for (name, p) in &anomalies.probs {
            if rng.next() < *p {
                summary.anomalies_fired += 1;
                state.counters.anomalies_fired += 1;
                let key = format!("{kind_slug}:{name}");
                *state
                    .counters
                    .anomalies_fired_by_kind
                    .entry(key)
                    .or_insert(0) += 1;
            }
        }
    }

    let subject = build_subject(subject_kind, subject_id);
    let title = format!("{} — {}", spec.label, subject_id);
    // Use `Uuid::new_v4()` (not the seeded-Rng path) so multiple
    // engine instances against the same persistent DB don't
    // collide on Job/Step IDs. Seeded-Rng UUIDs were originally
    // adopted for byte-perfect replay determinism, but the
    // determinism that matters for projection correctness lives
    // in the audit_log payload — not in the UUID values
    // themselves. Cross-run UUID collision was silently dropping
    // step rows via `ON CONFLICT (id) DO NOTHING`, leaving
    // brand-new Jobs with 2 of their 7 spec steps because the
    // other 5 IDs already pointed at older Jobs from a previous
    // engine instance.
    let job_id = JobId::new();
    // Realistic priority variance: most ops work is standard, with
    // a long tail of urgent / scheduled / emergency mirroring real
    // brewery operating distribution. Roll once per Job from the
    // seeded Rng so replay determinism stays intact.
    let priority = priority_for_kind(&spec.kind, rng);
    let mut job = Job::new(
        spec.kind.clone(),
        subject.clone(),
        title,
        "system-sim",
        priority,
        day,
    )
    .with_id(job_id)
    // The packet is born simulated — declared on the envelope, not
    // inferred from transport. Admission would also derive this from
    // the `x-sim-origin` header the LiveApiOutput stamps, but the
    // engine says it explicitly so the wire body carries the truth
    // even through a path that drops the header.
    .with_simulated(true);
    job.status = JobStatus::Open;

    // Post the Job and let the SERVER materialize its steps from the
    // Workflow step_graph — the server runs the open-time readiness pass
    // and emits `step.ready.<kind>` for the tier-0 steps, which the
    // workforce (and the notifier) pick up. The sim holds no step or
    // job mirror; the live system is the source of truth. Errors are
    // swallowed (the log is the system of record; replays are
    // idempotent). Serializing the full Job struct keeps the wire shape
    // in sync with `Json<Job>` on the receive side (the Subject's
    // #[serde(tag = "subject_kind")] flattens into the top-level body).
    let job_body = serde_json::to_value(&job).unwrap_or_else(|_| {
        serde_json::json!({
            "id": job_id.to_string(),
            "kind": job.kind,
            "subject_kind": SubjectTrait::kind(&subject),
            "id_": SubjectTrait::id(&subject),
            "title": job.title,
            "owner_id": job.owner_id,
            "status": "open",
            "priority": "standard",
            "opened_on": day.format("%Y-%m-%d").to_string(),
        })
    });
    let _ = output.emit_job_json(&job_body);

    bus.publish(
        "job.opened",
        HUMAN_WORKER_SOURCE,
        serde_json::json!({
            "job_id": job_id.to_string(),
            "kind": kind_slug,
            "subject_kind": subject_kind,
            "subject_id": subject_id,
            "opened_on": day.format("%Y-%m-%d").to_string(),
        }),
    );

    summary.jobs_created += 1;
    state.counters.jobs_created += 1;
    *summary.by_kind.entry(kind_slug.to_string()).or_insert(0) += 1;
    *state
        .counters
        .jobs_created_by_kind
        .entry(kind_slug.to_string())
        .or_insert(0) += 1;
    job_id
}

/// Open a Job in response to a request event (today's bus carries
/// `periodic.job_requested` payloads from PeriodicAction::OpenJob,
/// future BatchSpec preventive maintenance rules will publish on the same shape).
/// The payload must carry `workflow`, `subject_kind`, and
/// `subject_id`; everything else is looked up against `workflows`
/// and `tenant`. Anomalies fire the same way as for rate-driven
/// Jobs so a periodic-opened Job is indistinguishable downstream.
///
/// Returns `Ok(true)` if a Job was materialized, `Ok(false)` if
/// the request was malformed or named a Workflow not in the
/// registry. Counters land on `state.counters` exactly like
/// `simulate_day`'s rate-driven Job creation.
#[allow(clippy::too_many_arguments)]
pub fn open_job_from_request(
    payload: &serde_json::Value,
    workflows: &[WorkflowSpec],
    tenant: &TenantConfig,
    day: chrono::NaiveDate,
    state: &mut ShapeDrivenState,
    rng: &mut Rng,
    output: &mut dyn SimOutput,
    bus: &mut SimEventBus,
    step_registry: &StepRegistry,
) -> bool {
    // Required-field gating. A missing field is almost always a
    // misconfigured `[periodic.*]` open_job spec — the engine
    // used to silently drop these requests, which is how the
    // brewery's `quarterly-sales-tax` periodic ran for months
    // producing zero Jobs (caught 2026-05-06). Now: log loudly
    // + bump a counter so misconfiguration surfaces on the
    // first fire instead of only at audit time.
    let Some(workflow) = payload.get("workflow").and_then(|v| v.as_str()) else {
        state.counters.jobs_skipped_missing_field += 1;
        tracing::warn!(
            payload = ?payload,
            "open_job_from_request: payload missing `workflow` — \
             check your `[periodic.*]` action spec"
        );
        return false;
    };
    let Some(subject_kind) = payload.get("subject_kind").and_then(|v| v.as_str()) else {
        state.counters.jobs_skipped_missing_field += 1;
        tracing::warn!(
            workflow = %workflow,
            payload = ?payload,
            "open_job_from_request: payload missing `subject_kind` — \
             your `[periodic.*]` open_job action must set \
             `action.subject_kind` (e.g. \"location\", \"account\") \
             matching the Workflow's `subject_kinds` declaration"
        );
        return false;
    };
    let Some(subject_id) = payload.get("subject_id").and_then(|v| v.as_str()) else {
        state.counters.jobs_skipped_missing_field += 1;
        tracing::warn!(
            workflow = %workflow,
            subject_kind = %subject_kind,
            payload = ?payload,
            "open_job_from_request: payload missing `subject_id` — \
             your `[periodic.*]` open_job action must set \
             `action.subject_id` (e.g. \"loc-brewery-brewhouse\")"
        );
        return false;
    };
    let Some(spec) = workflows.iter().find(|jk| jk.kind == workflow) else {
        state.counters.jobs_skipped_unknown_kind += 1;
        tracing::warn!(
            workflow = %workflow,
            "open_job_from_request: unknown workflow — either the \
             Workflow isn't registered in the live registry, or your \
             `[periodic.*]` action.workflow has a typo"
        );
        return false;
    };

    let mut summary = DaySummary {
        day: Some(day),
        ..Default::default()
    };
    create_job_with_steps(
        spec,
        subject_kind,
        subject_id,
        day,
        state,
        &mut summary,
        workflow,
        tenant,
        rng,
        output,
        bus,
        step_registry,
    );
    true
}

/// Birth new Subjects per `tenant.subject_rates`. Each kind's rate
/// is a Poisson mean for the number of new Subjects to add that
/// day; ramps work the same way as for Workflows (latest ramp where
/// `date <= day` wins, falls back to base `rate`).
///
/// Subject ids are synthesized as `{kind}-{counter:05}` where the
/// counter is per-kind monotonic — replayable for a given seed
/// because the counter lives on `state` and increments on each
/// birth.
///
/// When the tenant's `[subject_rates.<kind>]` block sets
/// `created_event_kind`, the engine emits that topic per birth
/// with payload `{id, name, born_on}` so audit_log rebuilders
/// can reconstruct the synthesized Subject. Without it, the pool
/// grows silently — fine for tenants that don't audit-source
/// the kind, but dangling-FK integrity scans will flag any
/// downstream events that reference the silent ids (this is
/// what surfaced the brewery's `vendor-00001` problem in
/// 6cb1ad7).
#[allow(clippy::too_many_arguments)]
fn birth_subjects(
    tenant: &TenantConfig,
    day: chrono::NaiveDate,
    tick: &Tick,
    state: &mut ShapeDrivenState,
    rng: &mut Rng,
    output: &mut dyn SimOutput,
    summary: &mut DaySummary,
) {
    // Slug-sorted iteration → deterministic across replays.
    let mut kinds: Vec<(&String, &SubjectRate)> = tenant.subject_rates.iter().collect();
    kinds.sort_by(|a, b| a.0.cmp(b.0));

    for (kind, sr) in kinds {
        // Tick-aware Poisson: per-tick lambda = per-day-rate ×
        // tick.day_fraction(); summed over 1/day_fraction ticks
        // per sim-day = per-day rate. Per-day expected birth count
        // is invariant across granularities.
        let lambda = effective_subject_rate(sr, day) * tick.day_fraction();
        if lambda <= 0.0 {
            continue;
        }
        let count = poisson_sample(rng, lambda);
        for _ in 0..count {
            // Compute the would-be next id WITHOUT advancing the
            // counter or growing the pool. The order is:
            //   1. Try the canonical *.created emit (if a topic is set).
            //   2. On success, advance counter + grow pool + bump
            //      summary counters.
            //   3. On failure, leave everything unchanged so the next
            //      tick retries the same id slot.
            //
            // Pre-2026-05-04 the order was inverted: the pool grew
            // first and the emit was best-effort. A failed emit then
            // left a phantom id — pickable by Workflows but unbacked
            // by an audit_log row, which downstream events
            // (commerce.invoice.created, etc.) referenced as a
            // dangling FK. The 2026-05-04 subject audit of the live
            // playground found 187 such phantom account ids; the
            // canonical 12-month seed bundle had zero, confirming
            // this code path as the live-tick source.
            let candidate_counter = state
                .counters
                .subject_id_counter
                .get(kind)
                .copied()
                .unwrap_or(0)
                + 1;
            let id = format!("{kind}-{candidate_counter:05}");

            if let Some(topic) = sr.created_event_kind.as_deref() {
                let mut payload = synthesized_birth_payload(kind, &id, day);
                payload["born_on"] = serde_json::Value::String(day.format("%Y-%m-%d").to_string());
                if let Err(e) = output.emit_event(
                    topic,
                    &payload,
                    Some((crate::api_activity::ActorKind::Environment, "demand")),
                ) {
                    tracing::warn!(
                        kind = %kind,
                        topic = %topic,
                        id = %id,
                        error = %e,
                        "subject_birth emit_event failed; rolling back \
                         (next tick retries the same id slot)"
                    );
                    // Leave counter + pool untouched. The next
                    // poisson_sample tick will roll independently;
                    // if it still wants a birth, it'll attempt the
                    // same id again. Eventually-consistent rather
                    // than skipping.
                    continue;
                }
            }

            // Emit succeeded (or wasn't required) — commit the birth.
            *state
                .counters
                .subject_id_counter
                .entry(kind.to_string())
                .or_insert(0) += 1;
            state.seed_subject(kind, &id);
            summary.subjects_born += 1;
            state.counters.subjects_born += 1;
            *state
                .counters
                .subjects_born_by_kind
                .entry(kind.to_string())
                .or_insert(0) += 1;
        }
    }
}

/// Resolve the effective subject birth rate for `kind` on `day`.
/// Mirrors `effective_rate_for_day` for SubjectRate (no
/// weekday/weekend multipliers, just ramps).
fn effective_subject_rate(sr: &SubjectRate, today: chrono::NaiveDate) -> f64 {
    sr.ramp
        .iter()
        .filter(|r| r.date <= today)
        .max_by_key(|r| r.date)
        .map(|r| r.rate)
        .unwrap_or(sr.rate)
        .max(0.0)
}

/// Synthesize a payload satisfying the destination API endpoint's
/// schema for a freshly-born Subject. Per-kind switch keeps the
/// per-kind required fields in one place; the `id` and `name` are
/// always set from the synthesized id (the synthesized name reads
/// as a clear placeholder so operators editing it later see they
/// were filling in for a sim-birth row).
fn synthesized_birth_payload(kind: &str, id: &str, day: chrono::NaiveDate) -> serde_json::Value {
    // Pull a deterministic numeric seed from the id's trailing
    // digits. Used to pick a plausible name + city deterministically
    // — same id always lands on the same fake details across replays.
    let seed: usize = id
        .rsplit('-')
        .next()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(0);

    // 24 plausible US cities + state codes. Real cities, no PII.
    const CITIES: &[(&str, &str)] = &[
        ("Austin", "TX"),
        ("Portland", "OR"),
        ("Denver", "CO"),
        ("Seattle", "WA"),
        ("Boston", "MA"),
        ("Chicago", "IL"),
        ("Atlanta", "GA"),
        ("Nashville", "TN"),
        ("Phoenix", "AZ"),
        ("San Diego", "CA"),
        ("Minneapolis", "MN"),
        ("Pittsburgh", "PA"),
        ("Raleigh", "NC"),
        ("Charleston", "SC"),
        ("Madison", "WI"),
        ("Asheville", "NC"),
        ("Burlington", "VT"),
        ("Boise", "ID"),
        ("Richmond", "VA"),
        ("Albuquerque", "NM"),
        ("Spokane", "WA"),
        ("Tucson", "AZ"),
        ("Salem", "OR"),
        ("Lexington", "KY"),
    ];
    let (city, state) = CITIES[seed % CITIES.len()];

    match kind {
        "vendor" => {
            // 20 plausible craft-supplier name patterns.
            const PREFIXES: &[&str] = &[
                "Heritage",
                "Pacific",
                "Cascade",
                "Foundry",
                "Stonecreek",
                "Beacon",
                "Northwind",
                "Western",
                "Vintage Co.",
                "Continental",
                "Summit",
                "Riverbend",
                "Old Mill",
                "Anchor",
                "Atlas",
                "Sterling",
                "Brightwater",
                "Junction",
                "Capital",
                "Highland",
            ];
            const SUFFIXES: &[&str] = &[
                "Supply",
                "Trading Co.",
                "Industries",
                "Distributors",
                "Brokers",
                "Logistics",
                "Cooperative",
                "& Sons",
                "Imports",
                "Provisions",
            ];
            let prefix = PREFIXES[seed % PREFIXES.len()];
            let suffix = SUFFIXES[(seed / PREFIXES.len()) % SUFFIXES.len()];
            let name = format!("{prefix} {suffix}");
            let contact_first = [
                "Alex", "Jordan", "Sam", "Casey", "Morgan", "Riley", "Taylor", "Drew",
            ][seed % 8];
            let contact_last = [
                "Cole", "Hayes", "Price", "Vega", "Park", "Reed", "Lane", "Burke",
            ][(seed / 8) % 8];
            let domain_slug = name
                .to_ascii_lowercase()
                .replace(['&', '.', ',', '\''], "")
                .replace(' ', "");
            serde_json::json!({
                "id": id,
                "name": name,
                "contact_name": format!("{contact_first} {contact_last}"),
                "contact_email": format!("{}.{}@{}.example", contact_first.to_ascii_lowercase(), contact_last.to_ascii_lowercase(), domain_slug),
                "city": city,
                "state": state,
                "lead_time_days": 14,
                "payment_terms": "net-30",
                "category": "uncategorized",
            })
        }
        "account" => {
            // Brewery wholesale customers: bottle shops, restaurants,
            // bars, distributors. tier=silver — these are placeholders,
            // not curated tier-1 accounts.
            const PREFIXES: &[&str] = &[
                "Northside",
                "Eastside",
                "Riverside",
                "Highland",
                "Brookline",
                "Capitol",
                "Harbor",
                "Westgate",
                "Lakeshore",
                "Old Town",
                "Hillside",
                "Mainstreet",
                "Sunset",
                "Fox Hollow",
                "Cedar Park",
                "Stonebridge",
                "Maple Grove",
                "Ironside",
                "Crestview",
                "Beacon Hill",
            ];
            const SUFFIXES: &[&str] = &[
                "Bottle Shop",
                "Taphouse",
                "Beer Garden",
                "Pub",
                "Public House",
                "Brewing Co.",
                "Cellars",
                "Liquor Mart",
                "Wine & Beer",
                "Growler Bar",
            ];
            let prefix = PREFIXES[seed % PREFIXES.len()];
            let suffix = SUFFIXES[(seed / PREFIXES.len()) % SUFFIXES.len()];
            let name = format!("{prefix} {suffix}");
            let director_first = [
                "Avery", "Quinn", "Skyler", "Reese", "Parker", "Logan", "Emerson", "Rowan",
            ][seed % 8];
            let director_last = [
                "Bennett", "Foster", "Hale", "Marsh", "Quigley", "Tran", "Walker", "Ortiz",
            ][(seed / 8) % 8];
            serde_json::json!({
                "id": id,
                "name": name,
                "director": format!("{director_first} {director_last}"),
                "city": city,
                "state": state,
                "tier": "silver",
                "customer_since": day.format("%Y-%m-%d").to_string(),
                // Round-robin through the brewery's seeded sales-rep
                // pool (emp-aa-284..emp-aa-293) so sim-birth accounts
                // get a plausible territory owner instead of the CTO
                // (which was a placeholder that surfaced in operator
                // testing).
                "territory_rep_id": format!("emp-aa-{:03}", 284 + (seed % 10)),
                "account_type": "wholesale",
                "contacts": [],
            })
        }
        // Other kinds: keep the minimal envelope. The unknown-kind
        // tenants will see a 4xx in the WARN log and can extend.
        _ => serde_json::json!({
            "id": id,
            "name": format!("Subject {id}"),
        }),
    }
}

/// Roll fresh shocks (at quarter starts) and prune expired ones.
///
/// Quarter starts are calendar Jan 1 / Apr 1 / Jul 1 / Oct 1 — the
/// fiscal-quarter convention most US tenants use. Each `ShockSpec`
/// rolls independently, so a quarter can have zero shocks or many.
///
/// Pruning is unconditional (runs every day) so an end_date that
/// falls on a non-quarter-boundary still gets cleared.
fn roll_and_prune_shocks(
    tenant: &TenantConfig,
    day: chrono::NaiveDate,
    state: &mut ShapeDrivenState,
    rng: &mut Rng,
) {
    use chrono::Datelike;

    state.active_shocks.retain(|s| s.end_date >= day);

    let is_quarter_start = matches!((day.month(), day.day()), (1, 1) | (4, 1) | (7, 1) | (10, 1));
    if !is_quarter_start {
        return;
    }

    for spec in &tenant.shock {
        if rng.next() >= spec.probability_per_quarter {
            continue;
        }
        // Resolve the target. Patterns pick one matching subject_id
        // out of the live pool at activation time so a "viral bar"
        // shock locks onto one specific account for its window.
        let resolved = match (&spec.target.subject_id, &spec.target.subject_id_pattern) {
            (Some(id), _) => crate::shape_driven::tenant::ShockTarget {
                kind: spec.target.kind.clone(),
                subject_id: Some(id.clone()),
                subject_id_pattern: None,
                counterparty: spec.target.counterparty.clone(),
            },
            (None, Some(pat)) => {
                let prefix = pat.strip_suffix('*').unwrap_or(pat);
                let candidates: Vec<&String> = state
                    .subjects
                    .values()
                    .flatten()
                    .filter(|id| id.starts_with(prefix))
                    .collect();
                if candidates.is_empty() {
                    continue;
                }
                let pick = (rng.next() * candidates.len() as f64) as usize;
                let id = candidates[pick.min(candidates.len() - 1)].clone();
                crate::shape_driven::tenant::ShockTarget {
                    kind: spec.target.kind.clone(),
                    subject_id: Some(id),
                    subject_id_pattern: None,
                    counterparty: spec.target.counterparty.clone(),
                }
            }
            (None, None) => spec.target.clone(),
        };
        let end_date = day + chrono::Duration::weeks(spec.duration_weeks as i64);
        state
            .active_shocks
            .push(crate::shape_driven::state::ActiveShock {
                name: spec.name.clone(),
                target: resolved,
                rate_multiplier: spec.rate_multiplier,
                end_date,
            });
    }
}

/// Resolve the multiplier for a Workflow + optional Subject draw.
/// Returns the product of every active shock that matches.
/// `subject_id == None` matches kind-wide shocks only.
fn active_shock_multiplier(state: &ShapeDrivenState, kind: &str, subject_id: Option<&str>) -> f64 {
    let mut mult = 1.0;
    for shock in &state.active_shocks {
        let matches = match subject_id {
            None => shock.target.kind.as_deref() == Some(kind) && shock.target.subject_id.is_none(),
            Some(id) => shock.target.matches(kind, id),
        };
        if matches {
            mult *= shock.rate_multiplier;
        }
    }
    mult
}

/// Wrap `count_jobs_for_day` with a multiplier. Implemented inline
/// rather than in `sampler.rs` so the shock concept stays in the
/// engine — the sampler stays purely a function of JobRate.
///
/// Deterministic rates short-circuit Poisson: the count is the
/// rounded `effective_rate × multiplier`, fired once per sim-day on
/// the `is_first_in_day` tick (day-anchored, unscaled). A shock
/// still bites — a 0.5× `supply-shortage` halves the rounded review
/// count, a 0.0× one suppresses it — but the daily review never
/// drops to zero by chance the way a Poisson(λ) draw can.
fn count_jobs_with_multiplier(
    jr: &crate::shape_driven::tenant::JobRate,
    today: chrono::NaiveDate,
    tick: &Tick,
    us_banking: &boss_core::calendar::BusinessCalendar,
    rng: &mut Rng,
    multiplier: f64,
) -> u32 {
    if jr.deterministic {
        if !tick.is_first_in_day {
            return 0;
        }
        let effective = crate::shape_driven::sampler::effective_rate_for_day(jr, today, us_banking)
            * multiplier;
        if effective <= 0.0 {
            return 0;
        }
        return effective.round() as u32;
    }
    let lambda = crate::shape_driven::sampler::effective_rate_for_day(jr, today, us_banking)
        * multiplier
        * tick.day_fraction();
    if lambda <= 0.0 {
        return 0;
    }
    poisson_sample(rng, lambda)
}

/// Translate a per-subject shock multiplier into a draw count.
///
/// - `multiplier == 1.0` → 1 (the original draw, untouched).
/// - `multiplier == 2.0` → always 2 (double).
/// - `multiplier == 0.5` → 1 with prob 0.5, else 0 (drop the draw).
/// - General case: floor + a Bernoulli on the fractional part.
fn expand_shock_draws(multiplier: f64, rng: &mut Rng) -> u32 {
    if (multiplier - 1.0).abs() < f64::EPSILON {
        return 1;
    }
    let whole = multiplier.floor() as u32;
    let frac = multiplier - whole as f64;
    let bonus = if frac > 0.0 && rng.next() < frac {
        1
    } else {
        0
    };
    whole + bonus
}

/// Pick one Subject id from the state's pool for `kind`. If the
/// JobRate has a `subject_distribution`, use it (weighted draw);
/// otherwise uniform across the active pool.
fn pick_subject(
    state: &ShapeDrivenState,
    kind: &str,
    distribution: &std::collections::HashMap<String, f64>,
    rng: &mut Rng,
) -> Option<String> {
    let pool = state.subjects.get(kind)?;
    if pool.is_empty() {
        return None;
    }
    if !distribution.is_empty() {
        // Weighted draw. Distribution sums to <= 1.0; the leftover
        // probability falls back to uniform over `pool`.
        //
        // CRITICAL: HashMap iteration order is non-deterministic
        // across runs (Rust's RandomState seeds the hasher per
        // process). A weighted draw that walks `distribution`
        // directly produces different selections on identical
        // replays, breaking correctness-protocol property 5.
        // Sort by id before iterating so the cumulative-weight
        // walk is replay-stable.
        let r = rng.next();
        let mut acc = 0.0;
        let mut sorted: Vec<(&String, &f64)> = distribution.iter().collect();
        sorted.sort_by(|a, b| a.0.cmp(b.0));
        for (id, weight) in sorted {
            acc += weight;
            if r < acc {
                // Honor the distribution even if the id isn't in
                // the active pool — caller is responsible for
                // keeping the two in sync.
                return Some(id.clone());
            }
        }
        // Fall through to uniform.
    }
    let idx = (rng.next() * pool.len() as f64) as usize;
    Some(pool[idx.min(pool.len() - 1)].clone())
}

fn build_subject(kind: &str, id: &str) -> Subject {
    match kind {
        "asset" => Subject::new("asset", id),
        "account" => Subject::new("account", id),
        "campaign" => Subject::new("campaign", id),
        "employee" => Subject::new("employee", id),
        "vendor" => Subject::new("vendor", id),
        "location" => Subject::new("location", id),
        // Tenant-defined / unknown kinds ride on Custom for now.
        // Once the SubjectKind registry's Phase C lifts the
        // hardcoded core list, this match collapses to a single
        // branch reading from the registry.
        other => Subject::new(other, id),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use boss_jobs::registry::{StepSpec, Terminal, WorkflowSpec};

    fn brewery_tenant() -> TenantConfig {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("..")
            .join("examples/brewery/seeds/tenant.toml");
        TenantConfig::load(&path).expect("brewery tenant.toml parses")
    }

    /// Rate-test Workflow: no steps. A degenerate (stepless) Workflow
    /// opens and immediately closes via the `no_steps` short-circuit
    /// in `create_job_with_steps` — exactly what the Job-creation-rate
    /// tests want (they assert on counts, not step lifecycle).
    fn jk(kind: &str, label: &str, subject_kinds: &[&str]) -> WorkflowSpec {
        WorkflowSpec::platform_seed(
            kind,
            label,
            "test",
            subject_kinds.iter().map(|s| s.to_string()).collect(),
            vec![],
        )
    }

    /// v2 flat StepSpec helper — one step `title`/`kind`/`ready_when`
    /// with all other fields defaulted.
    fn step_spec(title: &str, kind: &str, ready_when: &str) -> StepSpec {
        StepSpec {
            title: title.into(),
            kind: kind.into(),
            ready_when: ready_when.into(),
            ..Default::default()
        }
    }

    fn date(y: i32, m: u32, d: u32) -> chrono::NaiveDate {
        chrono::NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    /// Smoke test: morning-brew fires daily on its loc-brewery-
    /// central-kitchen Subject. A single Poisson(1.0) draw can be
    /// 0 ~37% of the time, so we run 7 days and assert ≥ 4 Jobs
    /// landed on the Location — robust to RNG variance, still
    /// catches the "zero Jobs ever created" failure mode.
    #[test]
    fn morning_bake_fires_against_seeded_location() {
        let tenant = brewery_tenant();
        let registry = StepRegistry::v1();
        let mut output = crate::output::InMemoryOutput::default();
        let kinds = vec![jk("morning-brew", "Morning Brew", &["location"])];

        let mut state = ShapeDrivenState::new();
        state.seed_subject("location", "loc-brewery-brewhouse");
        // tenant.toml's job_rates iterate ALL kinds; rates that
        // target subject pools the test doesn't seed bump
        // jobs_skipped_no_subject and break the assertion below.
        // Seed the catch-alls so unknown-to-this-test rates skip
        // cleanly via jobs_skipped_unknown_kind instead.
        state.seed_subject("account", "acc-direct-shop");
        state.seed_subject("employee", "emp-aa-100");
        let mut rng = Rng::new(tenant.meta.seed);

        let mut day = date(2024, 1, 8);
        for _ in 0..7 {
            simulate_day(
                &kinds,
                &registry,
                &tenant,
                day,
                &mut state,
                &mut rng,
                &mut output,
                &mut crate::engines::SimEventBus::new(),
            );
            day = day.succ_opt().unwrap();
        }

        // tenant.toml's job_rates iterate alphabetically; rates that
        // come before morning-brew (brewery-hire, brewery-terminate,
        // direct-shop-order, equipment-preventive-maintenance, ingredient-restock) each
        // consume RNG draws, so the morning-brew sub-sequence shifts
        // when those rates change. Bound is wide.
        let mb = *state
            .counters
            .jobs_created_by_kind
            .get("morning-brew")
            .unwrap_or(&0);
        assert!(
            (1..=12).contains(&mb),
            "morning-brew over 7 days at rate 1.0/day should land 1–12 \
             Jobs, got {mb}"
        );
        assert!(
            state
                .counters
                .jobs_created_by_kind
                .contains_key("morning-brew")
        );
    }

    /// Workflows the registry doesn't know about don't panic — they
    /// get counted as `unknown_kind` and the run proceeds.
    #[test]
    fn unknown_jobkind_falls_through() {
        let tenant = brewery_tenant();
        let registry = StepRegistry::v1();
        let mut output = crate::output::InMemoryOutput::default();
        // Empty registry — every tenant rate is "unknown" relative
        // to it.
        let kinds: Vec<WorkflowSpec> = vec![];

        let mut state = ShapeDrivenState::new();
        let mut rng = Rng::new(123);

        let summary = simulate_day(
            &kinds,
            &registry,
            &tenant,
            date(2024, 6, 1),
            &mut state,
            &mut rng,
            &mut output,
            &mut crate::engines::SimEventBus::new(),
        );
        assert_eq!(summary.jobs_created, 0);
        assert!(summary.jobs_skipped_unknown_kind > 0);
    }

    /// Workflows whose subject pool is empty don't fabricate — they
    /// count as `skipped_no_subject` and the test surfaces the
    /// missing seed. Run 7 days so a Poisson(1.0) draw of 0 on
    /// any single day doesn't false-negative.
    #[test]
    fn empty_subject_pool_skips_jobs() {
        let tenant = brewery_tenant();
        let registry = StepRegistry::v1();
        let mut output = crate::output::InMemoryOutput::default();
        let kinds = vec![jk("morning-brew", "Morning Brew", &["location"])];
        // No Locations seeded.
        let mut state = ShapeDrivenState::new();
        let mut rng = Rng::new(456);
        let mut total_created = 0u64;
        let mut total_skipped = 0u64;
        for d in 0..7 {
            let summary = simulate_day(
                &kinds,
                &registry,
                &tenant,
                date(2024, 1, 8) + chrono::Duration::days(d),
                &mut state,
                &mut rng,
                &mut output,
                &mut crate::engines::SimEventBus::new(),
            );
            total_created += summary.jobs_created;
            total_skipped += summary.jobs_skipped_no_subject;
        }
        assert_eq!(total_created, 0);
        assert!(
            total_skipped > 0,
            "7 days of Poisson(1.0) with no subjects should produce ≥1 skip"
        );
    }

    /// Per-Subject cadence: a JobRate with a `subject_cadence` row
    /// pinned to Tuesday fires a Job for that subject every Tuesday
    /// regardless of the Poisson draw. Verifies (a) cadence Jobs
    /// land on the right subject, (b) clustering shows up on the
    /// declared weekday, and (c) cadence stacks on top of Poisson —
    /// total jobs ≥ cadence-only count.
    #[test]
    fn subject_cadence_fires_on_declared_weekday() {
        let tenant_path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("..")
            .join("examples/brewery/seeds/tenant.toml");
        let mut tenant = TenantConfig::load(&tenant_path).expect("brewery parses");
        // Strip every other rate so the test focuses on cadence.
        tenant.job_rates.clear();
        tenant.subject_rates.clear();
        tenant.anomalies.clear();
        tenant.job_rates.insert(
            "wholesale-keg-order".into(),
            crate::shape_driven::tenant::JobRate {
                rate: 0.0, // cadence-only, no Poisson noise
                ramp: vec![],
                weekday_multiplier: None,
                weekend_multiplier: None,
                subject_distribution: Default::default(),
                subject_cadence: vec![
                    crate::shape_driven::tenant::SubjectCadence {
                        subject_id: "acc-tue-bar".into(),
                        weekday: "Tue".into(),
                        biweekly_offset: None,
                        time_of_day: None,
                    },
                    crate::shape_driven::tenant::SubjectCadence {
                        subject_id: "acc-fri-bar".into(),
                        weekday: "Fri".into(),
                        biweekly_offset: None,
                        time_of_day: None,
                    },
                ],
                month_multipliers: Default::default(),
                deterministic: false,
            },
        );

        let registry = StepRegistry::v1();
        let mut output = crate::output::InMemoryOutput::default();
        let kinds = vec![jk(
            "wholesale-keg-order",
            "Wholesale keg order",
            &["account"],
        )];
        let mut state = ShapeDrivenState::new();
        state.seed_subjects("account", ["acc-tue-bar", "acc-fri-bar"]);
        let mut rng = Rng::new(0xcad);

        // Run a 14-day window starting Mon 2026-01-05. Expected:
        // 2 Tuesday jobs (06, 13) for acc-tue-bar + 2 Friday jobs
        // (09, 16) for acc-fri-bar = 4 cadence jobs total.
        let mut day = date(2026, 1, 5);
        for _ in 0..14 {
            simulate_day(
                &kinds,
                &registry,
                &tenant,
                day,
                &mut state,
                &mut rng,
                &mut output,
                &mut crate::engines::SimEventBus::new(),
            );
            day = day.succ_opt().unwrap();
        }

        let total = state
            .counters
            .jobs_created_by_kind
            .get("wholesale-keg-order")
            .copied()
            .unwrap_or(0);
        assert_eq!(
            total, 4,
            "expected 4 cadence jobs over 14 days, got {total}"
        );

        // With a cadence-only config (rate 0, no Poisson noise), a total
        // of exactly 4 over the 14-day window can only be the 2 Tuesday
        // fires (acc-tue-bar) + 2 Friday fires (acc-fri-bar) — the
        // per-weekday cadence is validated by the exact total.
    }

    /// Random shock: a probability=1.0 spec rolling at a quarter
    /// boundary lands an `ActiveShock` with the right end_date and
    /// the rate multiplier visibly bumps the day's count.
    #[test]
    fn shock_rolls_on_quarter_start_and_multiplies_rate() {
        let tenant_path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("..")
            .join("examples/brewery/seeds/tenant.toml");
        let mut tenant = TenantConfig::load(&tenant_path).expect("brewery parses");
        tenant.job_rates.clear();
        tenant.subject_rates.clear();
        tenant.anomalies.clear();
        tenant.shock.clear();
        tenant.job_rates.insert(
            "wholesale-keg-order".into(),
            crate::shape_driven::tenant::JobRate {
                rate: 5.0,
                ramp: vec![],
                weekday_multiplier: None,
                weekend_multiplier: None,
                subject_distribution: Default::default(),
                subject_cadence: Default::default(),
                month_multipliers: Default::default(),
                deterministic: false,
            },
        );
        // Probability 1.0 → fires on every quarter boundary.
        // duration_weeks=2 gives a clean 14-day window.
        tenant.shock.push(crate::shape_driven::tenant::ShockSpec {
            name: "supply-shortage".into(),
            probability_per_quarter: 1.0,
            target: crate::shape_driven::tenant::ShockTarget {
                kind: Some("wholesale-keg-order".into()),
                subject_id: None,
                subject_id_pattern: None,
                counterparty: None,
            },
            rate_multiplier: 0.0, // dampen all the way to zero
            duration_weeks: 2,
        });

        let registry = StepRegistry::v1();
        let mut output = crate::output::InMemoryOutput::default();
        let kinds = vec![jk(
            "wholesale-keg-order",
            "Wholesale keg order",
            &["account"],
        )];
        let mut state = ShapeDrivenState::new();
        state.seed_subjects("account", ["acc-1", "acc-2", "acc-3"]);
        let mut rng = Rng::new(0xb007);

        // 2026-04-01 is a Wednesday and a quarter start. Shock
        // fires; for 14 days it dampens rate * 0.0 = 0 jobs.
        let mut day = date(2026, 4, 1);
        for _ in 0..14 {
            simulate_day(
                &kinds,
                &registry,
                &tenant,
                day,
                &mut state,
                &mut rng,
                &mut output,
                &mut crate::engines::SimEventBus::new(),
            );
            day = day.succ_opt().unwrap();
        }
        let dampened = state
            .counters
            .jobs_created_by_kind
            .get("wholesale-keg-order")
            .copied()
            .unwrap_or(0);
        assert_eq!(
            dampened, 0,
            "supply-shortage 0x multiplier should suppress every Job, got {dampened}"
        );

        // After the 2-week window, shock expires and Poisson rate
        // resumes. Run another 14 days; we should see Jobs again.
        for _ in 0..14 {
            simulate_day(
                &kinds,
                &registry,
                &tenant,
                day,
                &mut state,
                &mut rng,
                &mut output,
                &mut crate::engines::SimEventBus::new(),
            );
            day = day.succ_opt().unwrap();
        }
        assert!(state.active_shocks.is_empty(), "shock should have expired");
        let resumed = state
            .counters
            .jobs_created_by_kind
            .get("wholesale-keg-order")
            .copied()
            .unwrap_or(0);
        assert!(
            resumed > 0,
            "rate should resume after shock expires (rate=5.0 over 14 days), got {resumed}"
        );
    }

    /// Tenant operating-days filter — brewery's tenant.toml lists
    /// all 7 days as operating, so this test injects a 5-day-only
    /// tenant and asserts weekend Jobs are skipped.
    #[test]
    fn operating_days_filter_suppresses_jobs() {
        let mut tenant = brewery_tenant();
        let registry = StepRegistry::v1();
        let mut output = crate::output::InMemoryOutput::default();
        tenant.meta.operating_days = vec![
            "mon".into(),
            "tue".into(),
            "wed".into(),
            "thu".into(),
            "fri".into(),
        ];
        let kinds = vec![jk("morning-brew", "Morning Brew", &["location"])];
        let mut state = ShapeDrivenState::new();
        state.seed_subject("location", "loc-brewery-brewhouse");
        let mut rng = Rng::new(789);

        // Saturday 2024-01-06 — closed brewery
        let saturday = date(2024, 1, 6);
        assert_eq!(saturday.format("%a").to_string(), "Sat");
        let summary = simulate_day(
            &kinds,
            &registry,
            &tenant,
            saturday,
            &mut state,
            &mut rng,
            &mut output,
            &mut crate::engines::SimEventBus::new(),
        );
        assert_eq!(summary.jobs_created, 0);
    }

    /// 30-day brewery sim, full registry + subject pool. Asserts the
    /// per-kind counts roughly track the tenant rates. This is the
    /// "good test of how easily we can pivot" the user named at the
    /// start of the sim-rewrite work — brewery seeds + this engine
    /// = plausible brewery activity, no Rust changes.
    #[test]
    fn thirty_day_brewery_run_produces_plausible_counts() {
        let tenant = brewery_tenant();
        let registry = StepRegistry::v1();
        let mut output = crate::output::InMemoryOutput::default();
        let kinds = vec![
            jk("morning-brew", "Morning Brew", &["location"]),
            jk("morning-brew-ipa", "Morning Brew (IPA)", &["location"]),
            jk("morning-brew-stout", "Morning Brew (Stout)", &["location"]),
            jk("morning-brew-lager", "Morning Brew (Lager)", &["location"]),
            jk(
                "morning-brew-hazy",
                "Morning Brew (Hazy IPA)",
                &["location"],
            ),
            jk("wholesale-keg-order", "Wholesale Keg Order", &["account"]),
            jk("direct-shop-order", "Direct Shop Order", &["account"]),
            jk("seasonal-release", "Seasonal Release", &["account"]),
            jk("ingredient-restock", "Ingredient Restock", &["vendor"]),
            jk(
                "equipment-preventive-maintenance",
                "Equipment preventive maintenance",
                &["location"],
            ),
            jk("tap-launch", "Tap Launch", &["campaign"]),
            jk("brewery-hire", "Brewery Hire", &["employee"]),
            jk("brewery-terminate", "Brewery Terminate", &["employee"]),
            // Periodic / cadence Workflows fired by tenant.toml's
            // [periodic.*] / [batch.*] sections on the brewhouse
            // location — include them so the 30-day run has zero
            // unknown-kind skips.
            jk("ap-payment-run", "AP Payment Run", &["location"]),
            jk("payroll-run", "Payroll Run", &["location"]),
            jk("payroll-941-filing", "Payroll 941 Filing", &["location"]),
            jk("income-tax-filing", "Income Tax Filing", &["location"]),
            jk("sales-tax-filing", "Sales Tax Filing", &["location"]),
            // The-roster-acts operating rhythms (1533872f) — the
            // department shifts that wake the formerly dormant roles.
            jk(
                "taproom-service-shift",
                "Taproom Service Shift",
                &["location"],
            ),
            jk("route-delivery-run", "Route Delivery Run", &["location"]),
            jk("cellar-daily-round", "Cellar Daily Round", &["location"]),
            jk(
                "facility-maintenance-round",
                "Facility Maintenance Round",
                &["location"],
            ),
            jk("it-support-shift", "IT Support Shift", &["location"]),
            jk("account-checkin", "Account Check-in", &["account"]),
            jk("taproom-event", "Taproom Event", &["location"]),
            jk(
                "ar-collections-run",
                "Weekly A/R Collections Run",
                &["location"],
            ),
            jk(
                "weekly-leadership-review",
                "Weekly Leadership Review",
                &["location"],
            ),
            jk("people-ops-cycle", "Weekly People-Ops Cycle", &["location"]),
        ];

        let mut state = ShapeDrivenState::new();
        state.seed_subjects("location", ["loc-brewery-brewhouse", "loc-brewery-taproom"]);
        state.seed_subjects(
            "account",
            [
                "acc-mission-cafe",
                "acc-marina-grocer",
                "acc-soma-coworking",
            ],
        );
        state.seed_subjects("vendor", ["vnd-flour", "vnd-dairy", "vnd-packaging"]);
        state.seed_subjects("employee", ["emp-bk-001", "emp-bk-002"]);
        state.seed_subjects("campaign", ["cmp-spring-2024"]);

        let mut rng = Rng::new(tenant.meta.seed);
        let mut day = tenant.meta.start_date; // 2024-01-02
        for _ in 0..30 {
            simulate_day(
                &kinds,
                &registry,
                &tenant,
                day,
                &mut state,
                &mut rng,
                &mut output,
                &mut crate::engines::SimEventBus::new(),
            );
            day = day.succ_opt().unwrap();
        }

        // morning-brew (pale) is a deterministic daily production review
        // at 2 brew slots/working day (rate 2.0, weekday_multiplier 1.0,
        // weekend_multiplier 0.0): the high-volume kinds run 2 slots and
        // the gate decides brew-vs-skip per slot downstream. Window
        // 2024-01-02 → 2024-01-31 has 21 working days — 22 weekdays minus
        // MLK Day 2024-01-15 (a federal holiday the weekend_multiplier=0.0
        // rule suppresses) — so 2 × 21 = 42 reviews. The count is exact,
        // not Poisson — assert it tightly so a regression in the
        // deterministic path is loud.
        let mb = state
            .counters
            .jobs_created_by_kind
            .get("morning-brew")
            .copied()
            .unwrap_or(0);
        assert_eq!(
            mb, 42,
            "morning-brew (pale) is a deterministic 2-slot daily review: \
             2 × 21 working days in 2024-01-02..01-31 (MLK Day excluded), got {mb}"
        );

        // wholesale-order's launch ramp is rate=8.0/day starting
        // 2024-01-02 (the 2024-07-01 ramp to 18.0 hasn't happened
        // yet in the first 30 days), so over 30 days expect ~240.
        let wo = state
            .counters
            .jobs_created_by_kind
            .get("wholesale-keg-order")
            .copied()
            .unwrap_or(0);
        assert!(
            (180..=320).contains(&wo),
            "wholesale-order count should be ~240 over 30 days at rate 8.0, got {wo}"
        );

        // brewery-hire is RATE = 0 in tenant.toml today — the
        // Workflow's `subject_kinds = ["employee"]` shape picks an
        // existing employee as subject (wrong for a hire), which
        // produced noise + occasional duplicate Jobs. Tracked as
        // a TODO ("Fix duplicate executive hires" — sim-config
        // workaround, full rewrite deferred).
        let bh = state
            .counters
            .jobs_created_by_kind
            .get("brewery-hire")
            .copied()
            .unwrap_or(0);
        assert_eq!(
            bh, 0,
            "brewery-hire is rate=0 today; tenant.toml regression if non-zero, got {bh}"
        );

        // The-roster-acts rhythms (1533872f) are deterministic like
        // morning-brew, so their counts are exact. The taproom is open
        // weekends AND holidays (weekend_multiplier 1.0): 3 shift
        // sections × all 30 days. Distribution routes run business
        // days only: 2 × 21 working days (MLK Day excluded).
        let ts = state
            .counters
            .jobs_created_by_kind
            .get("taproom-service-shift")
            .copied()
            .unwrap_or(0);
        assert_eq!(
            ts, 90,
            "taproom-service-shift is a deterministic 3-section shift, \
             7 days a week: 3 × 30 days, got {ts}"
        );
        let rd = state
            .counters
            .jobs_created_by_kind
            .get("route-delivery-run")
            .copied()
            .unwrap_or(0);
        assert_eq!(
            rd, 42,
            "route-delivery-run is a deterministic 2-route dock day: \
             2 × 21 working days in 2024-01-02..01-31, got {rd}"
        );

        // No skips — every Workflow has a Subject pool seeded.
        assert_eq!(state.counters.jobs_skipped_no_subject, 0);
        // The `kinds` list above is the curated set this count test
        // asserts on; tenant.toml also fires a few cadence Workflows it
        // doesn't model (they open against seeded subjects but aren't in
        // the list), so a bounded number skip as unknown-kind. The full
        // 21-kind set is exercised end-to-end by the regen, not here.
        assert!(
            state.counters.jobs_skipped_unknown_kind < 120,
            "unexpected surge in unknown-kind skips ({}) — a curated kind \
             likely went missing or got renamed",
            state.counters.jobs_skipped_unknown_kind
        );
        assert_eq!(state.counters.days_simulated, 30);

        // This is a Job-creation rate test: it asserts only on what the
        // sim generates. The sim no longer drives or closes Jobs — the
        // workforce executor works them against the live system — so it
        // records zero step completions of its own.
        assert_eq!(state.counters.steps_completed, 0);
    }

    /// Subject birth: tenant.subject_rates drives Poisson-sampled
    /// new Subjects per day with monotonic per-kind ids. Brewery's
    /// account rate is 0.15/day → ~4-5 over 30 days. Vendor rate
    /// is 0.02/day → ~0-1 over 30 days. Asserts plausible counts
    /// + unique ids + that `state.subjects` grows accordingly.
    #[test]
    fn subjects_birth_at_configured_rates() {
        let tenant = brewery_tenant();
        let registry = StepRegistry::v1();
        let mut output = crate::output::InMemoryOutput::default();
        // No Workflows — focus the test on Subject birth alone.
        let kinds: Vec<WorkflowSpec> = vec![];

        let mut state = ShapeDrivenState::new();
        let mut rng = Rng::new(0xb1d75); // "births"

        let mut day = date(2024, 1, 2); // brewery start_date
        for _ in 0..30 {
            simulate_day(
                &kinds,
                &registry,
                &tenant,
                day,
                &mut state,
                &mut rng,
                &mut output,
                &mut crate::engines::SimEventBus::new(),
            );
            day = day.succ_opt().unwrap();
        }

        // Account rate 0.15/day over 30 days ⇒ Poisson mean 4.5.
        let accounts = state
            .counters
            .subjects_born_by_kind
            .get("account")
            .copied()
            .unwrap_or(0);
        assert!(
            (1..=12).contains(&accounts),
            "account births over 30 days at rate 0.15 should be 1-12, got {accounts}"
        );

        // Vendor rate 0.02/day ⇒ mean 0.6.
        let vendors = state
            .counters
            .subjects_born_by_kind
            .get("vendor")
            .copied()
            .unwrap_or(0);
        assert!(
            vendors <= 5,
            "vendor births over 30 days at rate 0.02 should be small, got {vendors}"
        );

        // Subjects landed in state.subjects with synthesized ids.
        let pool = state.subjects.get("account").cloned().unwrap_or_default();
        assert_eq!(pool.len() as u64, accounts);
        for id in &pool {
            assert!(
                id.starts_with("account-"),
                "expected account-* prefix, got {id}"
            );
        }

        // The id counter is monotonic and matches the count.
        assert_eq!(
            state
                .counters
                .subject_id_counter
                .get("account")
                .copied()
                .unwrap_or(0),
            accounts
        );

        // employee rate 0.0 ⇒ no births. The brewery overrides
        // employee birth specifically because hires happen via the
        // brewery-hire Workflow, not as standalone Subject creation.
        assert_eq!(
            state
                .counters
                .subjects_born_by_kind
                .get("employee")
                .copied()
                .unwrap_or(0),
            0,
        );

        // Birth emission: each born Subject of a kind whose
        // tenant.toml sets `created_event_kind` produces one
        // `output.emit_event(topic, …)` call. Brewery wires
        // account → accounts.account.created and vendor →
        // inventory.vendor.created. The two counts must match
        // the births so audit_log replay reconstructs the pool.
        let account_emits = output
            .events
            .iter()
            .filter(|(t, _)| t == "accounts.account.created")
            .count() as u64;
        let vendor_emits = output
            .events
            .iter()
            .filter(|(t, _)| t == "inventory.vendor.created")
            .count() as u64;
        assert_eq!(
            account_emits, accounts,
            "every account birth must emit one accounts.account.created event \
             (births={accounts}, emits={account_emits})",
        );
        assert_eq!(
            vendor_emits, vendors,
            "every vendor birth must emit one inventory.vendor.created event \
             (births={vendors}, emits={vendor_emits})",
        );

        // Payload shape: AccountWithContacts uses
        // `#[serde(flatten)]` so the account fields are flat at
        // the top level alongside `contacts`. `born_on` is added
        // so rebuilders can date the synthesized row.
        for (_, payload) in output
            .events
            .iter()
            .filter(|(t, _)| t == "accounts.account.created")
        {
            let id = payload.get("id").and_then(|v| v.as_str()).unwrap_or("");
            assert!(
                id.starts_with("account-"),
                "birth-event payload.id should start with `account-`, got {id}",
            );
            assert!(
                payload.get("contacts").and_then(|v| v.as_array()).is_some(),
                "AccountWithContacts birth payload needs contacts: []",
            );
            assert!(
                payload.get("territory_rep_id").is_some(),
                "synthesized account birth needs territory_rep_id default",
            );
            assert!(
                payload.get("born_on").is_some(),
                "birth-event payload should carry born_on for replay-dating",
            );
        }
        // Vendor payload is a flat shape (matches `POST
        // /api/inventory/vendors` CreateVendorRequest).
        for (_, payload) in output
            .events
            .iter()
            .filter(|(t, _)| t == "inventory.vendor.created")
        {
            let id = payload.get("id").and_then(|v| v.as_str()).unwrap_or("");
            assert!(
                id.starts_with("vendor-"),
                "vendor-birth payload.id should start with `vendor-`, got {id}",
            );
            assert_eq!(
                payload.get("payment_terms").and_then(|v| v.as_str()),
                Some("net-30"),
                "vendor-birth payload should carry the synthesized payment_terms default",
            );
        }
    }

    /// SubjectKind without `created_event_kind` set must NOT
    /// emit any audit event on birth — the field is the explicit
    /// opt-in, not the default. Used-device-shop's `system` kind
    /// is the canonical case (system creation runs through the
    /// catalog API path, not through the sim-engine output
    /// adapter).
    #[test]
    fn subject_birth_without_topic_emits_no_event() {
        use crate::shape_driven::tenant::SubjectRate;

        let mut tenant = brewery_tenant();
        tenant.subject_rates.clear();
        // High rate so the test reliably triggers ≥1 birth, but
        // no created_event_kind set.
        tenant.subject_rates.insert(
            "account".to_string(),
            SubjectRate {
                rate: 5.0,
                ramp: vec![],
                created_event_kind: None,
            },
        );

        let registry = StepRegistry::v1();
        let mut output = crate::output::InMemoryOutput::default();
        let kinds: Vec<WorkflowSpec> = vec![];
        let mut state = ShapeDrivenState::new();
        let mut rng = Rng::new(0xb1d75);

        let mut day = date(2024, 1, 2);
        for _ in 0..10 {
            simulate_day(
                &kinds,
                &registry,
                &tenant,
                day,
                &mut state,
                &mut rng,
                &mut output,
                &mut crate::engines::SimEventBus::new(),
            );
            day = day.succ_opt().unwrap();
        }

        let births = state
            .counters
            .subjects_born_by_kind
            .get("account")
            .copied()
            .unwrap_or(0);
        assert!(
            births > 0,
            "test setup error: rate=5.0 over 10 days should produce some births",
        );
        // No `accounts.account.created` events — the topic was
        // not wired, so the engine grew the pool silently.
        let emits = output
            .events
            .iter()
            .filter(|(t, _)| t.starts_with("accounts."))
            .count();
        assert_eq!(
            emits, 0,
            "subjects with no created_event_kind must not emit; got {emits}",
        );
    }

    /// Anomaly injection: the brewery's tenant.toml declares
    /// per-Workflow anomaly probabilities. Each Job-open rolls
    /// every probability for that Workflow once; hits increment the
    /// `anomalies_fired` counter. v0 doesn't yet translate fires
    /// into structural state changes — that's anomaly-handling
    /// Stage 6. This test pins the count path.
    #[test]
    fn anomalies_fire_at_configured_probabilities() {
        let tenant = brewery_tenant();
        let registry = StepRegistry::v1();
        let mut output = crate::output::InMemoryOutput::default();
        let kinds = vec![jk("seasonal-release", "Seasonal Release", &["account"])];

        let mut state = ShapeDrivenState::new();
        state.seed_subjects("account", ["acc-1", "acc-2", "acc-3"]);
        let mut rng = Rng::new(0x0a1a_10a1); // "anomalies"

        let mut day = date(2024, 1, 6);
        for _ in 0..365 {
            simulate_day(
                &kinds,
                &registry,
                &tenant,
                day,
                &mut state,
                &mut rng,
                &mut output,
                &mut crate::engines::SimEventBus::new(),
            );
            day = day.succ_opt().unwrap();
        }

        // seasonal-release has recipe_revision_probability = 0.20
        // + cancel_probability = 0.05. Rate is 0.15/day modulated by
        // month_multipliers (Jan 0.5, Feb 0.7, …); over a year
        // expect ~50 Jobs and several anomaly rolls. Exact counts
        // depend on RNG; the assertion is "the path produces non-
        // zero hits". 180 days (was 60) was needed after the 2026-
        // 05-02 federal-holiday-as-non-business-day change in
        // sampler.rs — that shifts every other Workflow's RNG draws
        // on holidays, so the seasonal-release seed lands a longer
        // run of 0s within a 60-day window for some seeds. 365 days
        // (was 180) for the same reason after the-roster-acts
        // (1533872f) added two Poisson kinds (account-checkin,
        // taproom-event) whose daily draws shift the stream again.
        assert!(
            state.counters.anomalies_fired > 0,
            "expected some anomalies to fire over 365 days, got 0"
        );
        // Per-anomaly counters use "<jobkind>:<name>" keys.
        let revisions = state
            .counters
            .anomalies_fired_by_kind
            .get("seasonal-release:recipe_revision_probability")
            .copied()
            .unwrap_or(0);
        let cancels = state
            .counters
            .anomalies_fired_by_kind
            .get("seasonal-release:cancel_probability")
            .copied()
            .unwrap_or(0);
        assert!(
            revisions + cancels > 0,
            "expected at least one seasonal-release anomaly fire over 60 days; \
             got revisions={revisions} cancels={cancels}"
        );
    }

    /// Determinism — same seed + same inputs produces the same
    /// output. Without this, the brewery + tenant runs aren't replayable.
    #[test]
    fn run_is_deterministic_for_a_seed() {
        let tenant = brewery_tenant();
        let registry = StepRegistry::v1();
        let kinds = vec![jk("morning-brew", "Morning Brew", &["location"])];

        let mut state_a = ShapeDrivenState::new();
        let mut state_b = ShapeDrivenState::new();
        state_a.seed_subject("location", "loc-brewery-brewhouse");
        state_b.seed_subject("location", "loc-brewery-brewhouse");
        let mut rng_a = Rng::new(0xfeedface);
        let mut rng_b = Rng::new(0xfeedface);
        let mut output_a = crate::output::InMemoryOutput::default();
        let mut output_b = crate::output::InMemoryOutput::default();

        let mut day = date(2024, 1, 2);
        for _ in 0..10 {
            let s_a = simulate_day(
                &kinds,
                &registry,
                &tenant,
                day,
                &mut state_a,
                &mut rng_a,
                &mut output_a,
                &mut crate::engines::SimEventBus::new(),
            );
            let s_b = simulate_day(
                &kinds,
                &registry,
                &tenant,
                day,
                &mut state_b,
                &mut rng_b,
                &mut output_b,
                &mut crate::engines::SimEventBus::new(),
            );
            assert_eq!(s_a.jobs_created, s_b.jobs_created);
            assert_eq!(s_a.by_kind, s_b.by_kind);
            day = day.succ_opt().unwrap();
        }

        assert_eq!(state_a.counters.jobs_created, state_b.counters.jobs_created);
    }

    /// Each Job lifecycle transition publishes the matching event onto
    /// the SimEventBus. Counts must equal the RunCounters so the
    /// CounterpartyEngine downstream can rely on the bus as the source
    /// of truth for trigger events.
    #[test]
    fn human_worker_publishes_job_and_step_events_to_bus() {
        use crate::engines::SimEventBus;

        let tenant = brewery_tenant();
        let registry = StepRegistry::v1();

        // v2 chain: trigger → Mix → Bake (terminal). The trigger
        // auto-completes at open (no step.done); Mix + Bake complete
        // through advance_steps and each publish step.done.
        let steps = vec![
            step_spec("trigger", "trigger", "true"),
            step_spec("mix", "task", "steps.trigger.done"),
            StepSpec {
                title: "bake".into(),
                kind: "task".into(),
                ready_when: "steps.mix.done".into(),
                title_template: "Bake".into(),
                terminal: Some(Terminal {
                    outcome: "completed".into(),
                }),
                ..Default::default()
            },
        ];
        let mut spec = WorkflowSpec::platform_seed(
            "morning-brew",
            "Morning Brew",
            "production",
            vec!["location".into()],
            steps,
        );
        spec.subject_kinds = vec!["location".into()];
        let kinds = vec![spec];

        let mut state = ShapeDrivenState::new();
        state.seed_subject("location", "loc-brewery-brewhouse");
        let mut rng = Rng::new(0xB05);
        let mut output = crate::output::InMemoryOutput::default();
        let mut bus = SimEventBus::new();

        // Track every event ever emitted across days. The bus clears
        // each day, so we capture the snapshot before the next call.
        let mut all_events: Vec<crate::engines::SimBusEvent> = Vec::new();
        let mut day = date(2024, 1, 2);
        for _ in 0..30 {
            simulate_day(
                &kinds,
                &registry,
                &tenant,
                day,
                &mut state,
                &mut rng,
                &mut output,
                &mut bus,
            );
            all_events.extend(bus.events().iter().cloned());
            bus.clear_day();
            day = day.succ_opt().unwrap();
        }

        let opened = all_events
            .iter()
            .filter(|e| e.topic == "job.opened")
            .count();
        let closed = all_events
            .iter()
            .filter(|e| e.topic == "job.closed")
            .count();
        let stepped = all_events.iter().filter(|e| e.topic == "step.done").count();

        assert_eq!(
            opened as u64, state.counters.jobs_created,
            "every job creation must publish job.opened"
        );
        assert_eq!(
            closed as u64, state.counters.jobs_closed,
            "every job closure must publish job.closed"
        );
        assert_eq!(
            stepped as u64, state.counters.steps_completed,
            "every step completion must publish step.done"
        );

        // Source must always be the human-worker constant so a
        // downstream CounterpartyEngine can filter by source. Spot-
        // check on a representative event.
        let first_open = all_events
            .iter()
            .find(|e| e.topic == "job.opened")
            .expect("at least one job opened");
        assert_eq!(first_open.source, HUMAN_WORKER_SOURCE);
        assert_eq!(first_open.payload["kind"], "morning-brew");
        assert_eq!(first_open.payload["subject_kind"], "location");
    }

    // ---- Regression tests for the 2026-05-04 emit-then-grow fix ----
    //
    // Pre-fix `birth_subjects` advanced the counter + grew the pool
    // before attempting the create event. A failed `output.emit_event`
    // left a phantom id (in pool, no audit row). The fix inverts the
    // order: emit first, advance counter + grow pool only on success.
    //
    // These tests inject a `FailingOutput` that fails every emit_event,
    // then assert the engine's state stays at zero births / counter
    // unchanged.

    /// SimOutput wrapper that delegates everything to an inner
    /// InMemoryOutput EXCEPT emit_event, which always fails. Used
    /// to verify the engine doesn't grow the pool on a failed emit.
    struct FailingEmitOutput {
        inner: crate::output::InMemoryOutput,
    }
    impl FailingEmitOutput {
        fn new() -> Self {
            Self {
                inner: Default::default(),
            }
        }
    }
    impl crate::output::SimOutput for FailingEmitOutput {
        fn emit_event(
            &mut self,
            _topic: &str,
            _payload: &serde_json::Value,
            _source: Option<(crate::api_activity::ActorKind, &str)>,
        ) -> anyhow::Result<()> {
            anyhow::bail!("test: emit always fails")
        }
        fn emit_system_event(&mut self, e: &boss_assets::types::AssetEvent) -> anyhow::Result<()> {
            self.inner.emit_system_event(e)
        }
        fn emit_invoice(&mut self, i: &boss_commerce::types::Invoice) -> anyhow::Result<()> {
            self.inner.emit_invoice(i)
        }
        fn emit_shipment(&mut self, s: &boss_shipping::types::Shipment) -> anyhow::Result<()> {
            self.inner.emit_shipment(s)
        }
        fn emit_agreement(&mut self, a: &crate::output::ActiveAgreement) -> anyhow::Result<()> {
            self.inner.emit_agreement(a)
        }
        fn emit_purchase_order(
            &mut self,
            p: &crate::output::PurchaseOrderSnapshot,
        ) -> anyhow::Result<()> {
            self.inner.emit_purchase_order(p)
        }
        fn emit_account_note(
            &mut self,
            n: &crate::output::AccountNoteSnapshot,
        ) -> anyhow::Result<()> {
            self.inner.emit_account_note(n)
        }
        fn flush(&mut self) -> anyhow::Result<()> {
            self.inner.flush()
        }
    }

    #[test]
    fn subject_birth_failed_emit_does_not_grow_pool() {
        use crate::shape_driven::tenant::SubjectRate;

        let mut tenant = brewery_tenant();
        tenant.subject_rates.clear();
        tenant.subject_rates.insert(
            "account".to_string(),
            SubjectRate {
                rate: 5.0, // very high so a birth is virtually guaranteed
                ramp: vec![],
                created_event_kind: Some("accounts.account.created".into()),
            },
        );

        let mut state = ShapeDrivenState::new();
        let initial_pool = state.subjects.get("account").map(|v| v.len()).unwrap_or(0);
        let initial_counter = state
            .counters
            .subject_id_counter
            .get("account")
            .copied()
            .unwrap_or(0);
        let mut rng = Rng::new(0xfa11);
        let mut output = FailingEmitOutput::new();
        let mut summary = DaySummary::default();

        birth_subjects(
            &tenant,
            date(2024, 1, 2),
            &Tick::day(),
            &mut state,
            &mut rng,
            &mut output,
            &mut summary,
        );

        // Pool MUST NOT have grown — the emit failed, so the birth
        // was rolled back. Counter MUST NOT have advanced for the
        // same reason. summary.subjects_born MUST be 0.
        assert_eq!(
            state.subjects.get("account").map(|v| v.len()).unwrap_or(0),
            initial_pool,
            "failed emit grew the pool — phantom id created"
        );
        assert_eq!(
            state
                .counters
                .subject_id_counter
                .get("account")
                .copied()
                .unwrap_or(0),
            initial_counter,
            "failed emit advanced the counter — next birth would skip ids"
        );
        assert_eq!(
            summary.subjects_born, 0,
            "failed emit reported a birth in the summary"
        );
    }

    #[test]
    fn subject_birth_succeeds_when_topic_unset() {
        // The opt-in shape: when created_event_kind is None, no emit
        // is attempted, so growth proceeds unconditionally. Verifies
        // the failure path doesn't accidentally block the no-topic
        // case.
        use crate::shape_driven::tenant::SubjectRate;

        let mut tenant = brewery_tenant();
        tenant.subject_rates.clear();
        tenant.subject_rates.insert(
            "vendor".to_string(),
            SubjectRate {
                rate: 5.0,
                ramp: vec![],
                created_event_kind: None,
            },
        );

        let mut state = ShapeDrivenState::new();
        let mut rng = Rng::new(0xfa12);
        let mut output = FailingEmitOutput::new(); // would fail if called, but isn't
        let mut summary = DaySummary::default();

        birth_subjects(
            &tenant,
            date(2024, 1, 2),
            &Tick::day(),
            &mut state,
            &mut rng,
            &mut output,
            &mut summary,
        );

        assert!(
            summary.subjects_born > 0,
            "no-topic path should still grow the pool"
        );
        assert!(state.subjects.get("vendor").is_some_and(|v| !v.is_empty()));
    }
}
