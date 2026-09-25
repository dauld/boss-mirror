//! `jobs.agent_step_overdue` — a real-work step waiting on an AGENT past
//! the bound its workflow declares files an alarm (backlog 078ddcb0).
//!
//! THE CASE. Measured 2026-09-23 by the Finance W39 retro collect and
//! the /ux/finance audit: receive-a-payout packet 931c3bfe's `post` step
//! sat on `agent-claude` from 2026-09-21 01:10Z to 2026-09-23 16:46Z —
//! 63.6 hours — while the $1.00 it recorded sat in Cash in Transit.
//! `boss orient` listed it in MY WORK among ~180 ready steps, most of
//! them backlog builds and page audits, and nobody acted on it. A list
//! is not an alarm: nothing told a step that moves money or answers the
//! outside world apart from a backlog item, and nothing aged it into a
//! signal. An agent executes only when a session reads its queue, so a
//! step on an agent that no session reads waits in silence — the one
//! failure mode CLAUDE.md forbids.
//!
//! THE DECLARATION IS THE RULE'S ARGS — `hours.<workflow> = <n>`, one per
//! workflow whose agent-held steps are real work — the same idiom as
//! `cadence-silence-sweep-daily`'s `interval_minutes.<kind>`: the rule
//! row is registry data (append-only, versioned, editable through
//! `/api/dispatcher/rules` with no deploy). A workflow nobody declared is
//! not read at all, and that is deliberate: `backlog-item` holds ~200
//! agent steps days old, and an alarm per backlog build would be the
//! flat list again, shouted.
//!
//! WHAT "WAITED" IS MEASURED FROM. The last instant the PACKET provably
//! moved — the newest `completed_at` on any of its steps, else
//! `metadata.opened_at` (`jobs_age_out_step::last_moved`, one
//! definition). A step carries no stamp of the moment it became ready,
//! and it becomes ready when a step before it completes, so this is the
//! step's wait or LESS: a packet that moved on another branch reads
//! younger than its oldest waiting step, never older. The alarm can be
//! late; it cannot be false. Measured against 931c3bfe: its trigger
//! carries no `completed_at`, so its wait reads from `opened_at`
//! 01:10Z — the instant the post step was assigned.
//!
//! WHICH STEPS. `ready` or `active`, assigned to an id the agents
//! registry (`GET /api/agents`) names as an agent's id or alias. An
//! empty registry is refused, never read as "no agents": a dark or
//! narrowed read answers `total: 0` (`common::empty_roster_refusal`).
//!
//! ONE ALARM PER STEP, EVER. Each alarm is a `backlog-item` carrying the
//! step's id under [`STEP_KEY`], and the dedup read takes EVERY status:
//! a step that already has an alarm — open, withdrawn, or closed by a
//! person — is never alarmed again, so a person's close is final and no
//! tick nags. An OPEN alarm whose step no longer waits (completed,
//! reassigned to a person, its packet closed, its workflow no longer
//! declared) is withdrawn at the step the alarm waits on
//! (`common::retraction`), stamped `cleared_by` this handler.
//!
//! WHO READS IT. `boss orient` leads MY WORK with the open alarms naming
//! the caller (`boss-cli` `orient::overdue_lines`, reading the keys
//! below — one definition, shared by the crate that writes them).
//!
//! AN ALARM, NOT A LIMIT. Nothing here completes, claims, reassigns or
//! reorders the late step. It files a packet and takes it back.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use async_trait::async_trait;
use boss_dispatcher::rules::expr::Value;
use boss_dispatcher::rules::handler::{Handler, HandlerError, InvocationContext};
use boss_jobs::channels::InputChannel;
use chrono::{DateTime, Utc};
use serde_json::{Value as Json, json};

use super::common::{
    RECOVERED_AT, Retraction, api_client, complete_step, empty_roster_refusal, get_json,
    jobs_where, open_jobs_of_kind, owner_for_filing, post_json, recovery_note, retraction,
    rows_or_refuse, with_lane, withdrawal_fields, write_json,
};
use super::jobs_age_out_step::last_moved;

/// The handler's registered name.
pub const HANDLER: &str = "jobs.agent_step_overdue";

/// The arg prefix a declaration carries: `hours.<workflow>`.
pub const ARG_PREFIX: &str = "hours.";

/// The `scope` every alarm this handler files carries.
pub const SCOPE: &str = "overdue-agent-step";

/// The packet-metadata key holding the late step's id — the dedup key,
/// and the key `boss orient` narrows its read on (`metadata_has`).
pub const STEP_KEY: &str = "overdue_step";

/// The metadata keys the alarm carries for its reader, beside
/// [`STEP_KEY`]: the late step's packet, workflow, slug, assignee, and
/// the measurement.
pub const PACKET_KEY: &str = "overdue_packet";
pub const WORKFLOW_KEY: &str = "workflow";
pub const SLUG_KEY: &str = "step_slug";
pub const ASSIGNEE_KEY: &str = "assignee";
pub const TITLE_KEY: &str = "packet_title";
pub const WAITED_KEY: &str = "waited_hours";
pub const BOUND_KEY: &str = "bound_hours";

/// The key the schedule runner writes the firing instant under, on
/// every sub-day tick (`schedule_runner::tick`).
const TICK_AT: &str = "_at";

/// The declared bounds on this rule's args, workflow → hours, in
/// workflow order. Args without [`ARG_PREFIX`] are ignored; a prefixed
/// arg is ALWAYS a declaration, so one that is not a positive whole
/// number of hours refuses by name rather than vanishing — and a rule
/// that declares nothing refuses too, because a watch over nothing is a
/// check that is not running.
pub fn declarations(args: &[(String, Value)]) -> Result<BTreeMap<String, i64>, String> {
    let mut out = BTreeMap::new();
    for (key, value) in args {
        let Some(workflow) = key.strip_prefix(ARG_PREFIX) else {
            continue;
        };
        match value {
            Value::Int(n) if *n > 0 && !workflow.is_empty() => {
                out.insert(workflow.to_string(), *n);
            }
            other => {
                return Err(format!(
                    "`{key}` must be a positive whole number of hours for a named workflow, got \
                     a {} ({other:?})",
                    other.kind()
                ));
            }
        }
    }
    if out.is_empty() {
        return Err(format!(
            "no `{ARG_PREFIX}<workflow>` declaration on the rule — it watches nothing"
        ));
    }
    Ok(out)
}

/// Every id the agents registry names — each agent's id and aliases.
pub fn agent_identities(agents: &[Json]) -> BTreeSet<String> {
    agents
        .iter()
        .flat_map(|a| {
            let id = a.get("id").and_then(Json::as_str).map(str::to_string);
            let aliases = a
                .get("aliases")
                .and_then(Json::as_array)
                .into_iter()
                .flatten()
                .filter_map(Json::as_str)
                .map(str::to_string);
            id.into_iter().chain(aliases)
        })
        .filter(|s| !s.is_empty())
        .collect()
}

/// One step held by an agent past its workflow's bound.
#[derive(Debug, Clone, PartialEq)]
pub struct Overdue {
    pub packet: String,
    pub packet_title: String,
    pub workflow: String,
    pub step_id: String,
    pub slug: String,
    pub status: String,
    pub assignee: String,
    pub since: DateTime<Utc>,
    pub waited_hours: f64,
    pub bound_hours: i64,
}

/// The late steps among one declared workflow's open packets. Pure.
/// A packet whose age cannot be read is skipped — "I cannot tell how
/// old this is" is not "late" — and reported in the journal by the
/// caller's count.
pub fn overdue_steps(
    workflow: &str,
    bound_hours: i64,
    packets: &[Json],
    agents: &BTreeSet<String>,
    now: DateTime<Utc>,
) -> Vec<Overdue> {
    let mut out = Vec::new();
    for job in packets {
        let Some(since) = last_moved(job, None) else {
            continue;
        };
        let waited_hours = (now - since).num_seconds() as f64 / 3600.0;
        if waited_hours <= bound_hours as f64 {
            continue;
        }
        let text = |v: &Json, k: &str| v.get(k).and_then(Json::as_str).unwrap_or("").to_string();
        for step in job
            .get("steps")
            .and_then(Json::as_array)
            .into_iter()
            .flatten()
        {
            let status = text(step, "status");
            let assignee = text(step, "assignee_id");
            if !matches!(status.as_str(), "ready" | "active") || !agents.contains(&assignee) {
                continue;
            }
            out.push(Overdue {
                packet: text(job, "id"),
                packet_title: text(job, "title"),
                workflow: workflow.to_string(),
                step_id: text(step, "id"),
                slug: text(step, "spec_slug"),
                status,
                assignee,
                since,
                waited_hours: (waited_hours * 10.0).round() / 10.0,
                bound_hours,
            });
        }
    }
    out
}

/// The alarm one late step files.
pub fn alarm_body(o: &Overdue, owner: &str, now: DateTime<Utc>, rule: &str) -> Json {
    let id8 = &o.packet[..o.packet.len().min(8)];
    let detail = format!(
        "Raised by {HANDLER} (rule {rule}, backlog 078ddcb0) at {now}: step `{slug}` of \
         {workflow} packet {packet} ({title}) is {status} and assigned to {assignee}, and the \
         packet has not moved since {since} — {waited}h, past the {bound}h this rule declares \
         for {workflow}. An agent acts only when a session reads its queue, so a step on an \
         agent that no session reads waits in silence; this alarm is that read. Do the step — \
         `boss orient` lists it first under MY WORK. This alarm refuses nothing, and it is \
         withdrawn once the step no longer waits on an agent.",
        now = now.to_rfc3339(),
        slug = o.slug,
        workflow = o.workflow,
        packet = o.packet,
        title = o.packet_title,
        status = o.status,
        assignee = o.assignee,
        since = o.since.to_rfc3339(),
        waited = o.waited_hours,
        bound = o.bound_hours,
    );
    let metadata = json!({
        "area": "platform",
        "scope": SCOPE,
        STEP_KEY: o.step_id,
        PACKET_KEY: o.packet,
        WORKFLOW_KEY: o.workflow,
        SLUG_KEY: o.slug,
        ASSIGNEE_KEY: o.assignee,
        TITLE_KEY: o.packet_title,
        WAITED_KEY: o.waited_hours,
        BOUND_KEY: o.bound_hours,
        "waiting_since": o.since.to_rfc3339(),
        "measured_at": now.to_rfc3339(),
        "detail": detail,
    });
    json!({
        "kind": "backlog-item",
        "title": format!(
            "OVERDUE: {} `{}` has waited {}h on {} (bound {}h) — packet {id8}",
            o.workflow, o.slug, o.waited_hours, o.assignee, o.bound_hours
        ),
        "subject": {"subject_kind": "custom", "id": o.packet},
        "owner_id": owner,
        "priority": "urgent",
        "status": "open",
        "tags": [],
        "metadata": with_lane(metadata, InputChannel::Telemetry),
    })
}

/// Every alarm this handler has filed, keyed by the step it names.
pub fn alarms_by_step(rows: &[Json]) -> BTreeMap<String, Json> {
    rows.iter()
        .filter_map(|row| {
            let step = row
                .pointer(&format!("/metadata/{STEP_KEY}"))
                .and_then(Json::as_str)
                .filter(|s| !s.is_empty())?;
            Some((step.to_string(), row.clone()))
        })
        .collect()
}

pub struct JobsAgentStepOverdue {
    client: reqwest::Client,
    jobs_base: String,
    /// Who the alarms are owned by — the platform owner through the port
    /// (backlog 3c23662d); never a literal.
    owner: Arc<dyn boss_core::platform_owner::PlatformOwner>,
}

impl JobsAgentStepOverdue {
    pub fn new(
        jobs_base: impl Into<String>,
        owner: Arc<dyn boss_core::platform_owner::PlatformOwner>,
    ) -> Arc<Self> {
        Arc::new(Self {
            client: api_client(),
            jobs_base: jobs_base.into(),
            owner,
        })
    }

    fn base(&self) -> &str {
        self.jobs_base.trim_end_matches('/')
    }

    async fn withdraw(
        &self,
        step_id: &str,
        alarm: &Json,
        now: DateTime<Utc>,
        rule: &str,
    ) -> Result<&'static str, HandlerError> {
        let id = alarm.get("id").and_then(Json::as_str).unwrap_or_default();
        let evidence = format!(
            "{HANDLER} read the declared workflows' open packets at {}: step {step_id} is no \
             longer a ready or active step held by an agent past its bound (completed, \
             reassigned, its packet closed, or its workflow no longer declared).",
            now.to_rfc3339()
        );
        match retraction(alarm) {
            None => {
                tracing::warn!(rule, packet = %id, "{HANDLER}: the open alarm has no triage step to close");
                Ok("unclosable")
            }
            // The fields through the step merge door, then the flip
            // (e39a9d2a).
            Some(Retraction::Complete { step_id: sid, .. }) => {
                complete_step(
                    &self.client,
                    self.base(),
                    id,
                    &sid,
                    withdrawal_fields(&evidence, HANDLER, &now.to_rfc3339()),
                    rule,
                )
                .await?;
                Ok("withdrawn")
            }
            // A packet already told is not told again on every tick.
            Some(Retraction::Annotate { .. })
                if alarm
                    .pointer(&format!("/metadata/{RECOVERED_AT}"))
                    .is_some_and(|v| !v.is_null()) =>
            {
                Ok("told")
            }
            Some(Retraction::Annotate { why_open }) => {
                write_json(
                    &self.client,
                    reqwest::Method::PATCH,
                    &format!("{}/api/jobs/{id}/metadata", self.base()),
                    &Json::Object(recovery_note(
                        &evidence,
                        HANDLER,
                        &now.to_rfc3339(),
                        &why_open,
                    )),
                    rule,
                )
                .await?;
                Ok("told")
            }
        }
    }
}

#[async_trait]
impl Handler for JobsAgentStepOverdue {
    fn name(&self) -> &'static str {
        HANDLER
    }

    async fn invoke(
        &self,
        args: &[(String, Value)],
        ctx: &InvocationContext,
    ) -> Result<(), HandlerError> {
        let rule = ctx.rule_name.as_str();
        let declared = declarations(args).map_err(HandlerError::Permanent)?;
        // The tick's own instant. Absent on a daily firing — a bound of
        // hours judged once a day is the same silence one level up.
        let now = ctx
            .event_payload
            .get(TICK_AT)
            .and_then(Json::as_str)
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|t| t.with_timezone(&Utc))
            .ok_or_else(|| {
                HandlerError::Permanent(format!(
                    "the firing carries no `{TICK_AT}` — {HANDLER} judges hours and needs a \
                     sub-day cadence (hourly, every-<n>-minutes)"
                ))
            })?;

        let listing = get_json(&self.client, &format!("{}/api/agents", self.base()), rule).await?;
        let agents: Vec<Json> =
            rows_or_refuse(&listing, "GET /api/agents").map_err(HandlerError::Downstream)?;
        let agents = agent_identities(&agents);
        if agents.is_empty() {
            return Err(empty_roster_refusal(
                HANDLER,
                "agents",
                "every instance runs at least one registered agent, and an empty read is a dark \
                 or narrowed registry — judging it would call every agent-held step on time",
            ));
        }

        let mut late: Vec<Overdue> = Vec::new();
        for (workflow, bound) in &declared {
            let packets = open_jobs_of_kind(&self.client, self.base(), workflow, rule).await?;
            late.extend(overdue_steps(workflow, *bound, &packets, &agents, now));
        }

        // EVERY status: a step alarmed once is never alarmed again.
        let filed = alarms_by_step(
            &jobs_where(
                &self.client,
                self.base(),
                &format!("kind=backlog-item&metadata_has={STEP_KEY}"),
                rule,
            )
            .await?,
        );

        let owner = owner_for_filing(self.owner.as_ref(), rule).await;
        let mut raised = 0usize;
        for o in &late {
            if filed.contains_key(&o.step_id) {
                continue;
            }
            post_json(
                &self.client,
                &format!("{}/api/jobs", self.base()),
                &alarm_body(o, &owner, now, rule),
                rule,
            )
            .await?;
            raised += 1;
        }

        let still_late: BTreeSet<&str> = late.iter().map(|o| o.step_id.as_str()).collect();
        let mut withdrawn = 0usize;
        for (step_id, alarm) in &filed {
            let open = alarm.get("status").and_then(Json::as_str) == Some("open");
            if !open || still_late.contains(step_id.as_str()) {
                continue;
            }
            if self.withdraw(step_id, alarm, now, rule).await? == "withdrawn" {
                withdrawn += 1;
            }
        }
        tracing::info!(
            rule,
            late = late.len(),
            raised,
            withdrawn,
            "{HANDLER}: read {} declared workflow(s)",
            declared.len()
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::listing_stub;
    use super::*;

    const NOW: &str = "2026-09-23T16:00:00+00:00";

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn ctx() -> InvocationContext {
        InvocationContext {
            rule_name: "agent-held-real-work-is-watched-hourly".into(),
            triggering_event_id: format!("clock-tick:{NOW}"),
            triggering_topic: "clock.tick".into(),
            event_payload: json!({"_day": "2026-09-23", "_at": NOW}),
        }
    }

    fn args() -> Vec<(String, Value)> {
        vec![("hours.receive-a-payout".to_string(), Value::Int(4))]
    }

    fn agents() -> BTreeSet<String> {
        agent_identities(&[json!({"id": "agent-claude", "aliases": ["claude@algedonic.dev"]})])
    }

    /// A receive-a-payout packet in the shape 931c3bfe had: a trigger
    /// born completed with no stamp, and `post` on the agent.
    fn payout(id: &str, opened_at: &str, post_status: &str, assignee: &str) -> Json {
        json!({
            "id": id, "kind": "receive-a-payout", "status": "open",
            "title": format!("Payout {id}"),
            "metadata": {"opened_at": opened_at},
            "steps": [
                {"id": format!("{id}-paid"), "spec_slug": "paid", "status": "completed",
                 "completed_at": null},
                {"id": format!("{id}-post"), "spec_slug": "post", "status": post_status,
                 "assignee_id": assignee},
                {"id": format!("{id}-posted"), "spec_slug": "posted", "status": "pending"},
            ],
        })
    }

    fn listing(rows: Vec<Json>) -> Json {
        let n = rows.len();
        json!({"data": rows, "total": n})
    }

    fn alarm_row(step: &str, status: &str, triage: &str) -> Json {
        json!({
            "id": "al-1", "kind": "backlog-item", "status": status,
            "metadata": {"scope": SCOPE, STEP_KEY: step},
            "steps": [{"id": "al-1-triage", "spec_slug": "triage", "status": triage,
                       "metadata": {"authority_role": "platform-admin"}}],
        })
    }

    #[test]
    fn a_declaration_is_a_positive_whole_number_of_hours_for_a_named_workflow() {
        let ok = declarations(&[
            ("hours.receive-a-payout".into(), Value::Int(4)),
            ("unrelated".into(), Value::String("x".into())),
        ])
        .unwrap();
        assert_eq!(ok, BTreeMap::from([("receive-a-payout".to_string(), 4)]));
        for bad in [
            Value::Int(0),
            Value::Int(-3),
            Value::String("soon".into()),
            Value::Float(1.5),
        ] {
            let err = declarations(&[("hours.receive-a-payout".into(), bad.clone())]).unwrap_err();
            assert!(err.contains("hours.receive-a-payout"), "{bad:?}: {err}");
        }
        assert!(
            declarations(&[("hours.".into(), Value::Int(4))]).is_err(),
            "a declaration names its workflow"
        );
        assert!(
            declarations(&[]).unwrap_err().contains("watches nothing"),
            "a rule that declares nothing refuses"
        );
    }

    /// The case: the payout post, 63.6h on the agent against a 4h bound,
    /// is late; the same step on a person, a step not yet ready, and a
    /// packet inside its bound are not.
    #[test]
    fn only_a_ready_or_active_agent_step_past_the_bound_is_late() {
        let packets = vec![
            payout("late", "2026-09-21T01:10:00Z", "ready", "agent-claude"),
            payout(
                "alias",
                "2026-09-21T01:10:00Z",
                "active",
                "claude@algedonic.dev",
            ),
            payout("person", "2026-09-21T01:10:00Z", "ready", "emp-david"),
            payout("pending", "2026-09-21T01:10:00Z", "pending", "agent-claude"),
            payout("fresh", "2026-09-23T13:00:00Z", "ready", "agent-claude"),
            json!({"id": "ageless", "metadata": {}, "steps": [
                {"id": "x", "spec_slug": "post", "status": "ready", "assignee_id": "agent-claude"}]}),
        ];
        let late = overdue_steps("receive-a-payout", 4, &packets, &agents(), at(NOW));
        let ids: Vec<&str> = late.iter().map(|o| o.packet.as_str()).collect();
        assert_eq!(ids, vec!["late", "alias"], "{late:?}");
        assert_eq!(late[0].step_id, "late-post");
        assert_eq!(late[0].slug, "post");
        assert_eq!(late[0].waited_hours, 62.8);
        assert_eq!(late[0].since, at("2026-09-21T01:10:00Z"));
    }

    /// A packet that moved later reads younger: the wait is measured from
    /// its newest completion, so the alarm can be late but never false.
    #[test]
    fn the_wait_is_measured_from_the_packets_last_movement() {
        let mut p = payout("moved", "2026-09-21T01:10:00Z", "ready", "agent-claude");
        p["steps"][0]["completed_at"] = json!("2026-09-23T14:00:00Z");
        assert!(overdue_steps("receive-a-payout", 4, &[p], &agents(), at(NOW)).is_empty());
    }

    #[test]
    fn the_alarm_names_the_step_the_packet_the_wait_and_the_bound() {
        let o = &overdue_steps(
            "receive-a-payout",
            4,
            &[payout(
                "931c3bfe-e483",
                "2026-09-21T01:10:00Z",
                "ready",
                "agent-claude",
            )],
            &agents(),
            at(NOW),
        )[0];
        let body = alarm_body(o, "emp-owner", at(NOW), "r");
        assert_eq!(body["kind"], "backlog-item");
        assert_eq!(body["priority"], "urgent");
        assert_eq!(body["owner_id"], "emp-owner");
        let m = &body["metadata"];
        assert_eq!(m[STEP_KEY], "931c3bfe-e483-post");
        assert_eq!(m[PACKET_KEY], "931c3bfe-e483");
        assert_eq!(m[ASSIGNEE_KEY], "agent-claude");
        assert_eq!(m[BOUND_KEY], 4);
        assert_eq!(m["scope"], SCOPE);
        let title = body["title"].as_str().unwrap();
        assert!(
            title.starts_with("OVERDUE: receive-a-payout `post` has waited 62.8h on agent-claude"),
            "{title}"
        );
        assert!(
            m["detail"].as_str().unwrap().contains("boss orient"),
            "the alarm says where the step is listed"
        );
    }

    // -- end to end, against the stub jobs API ---------------------------

    const AGENTS: &str = "/api/agents";
    const OPEN: &str = "/api/jobs?kind=receive-a-payout&status=open";
    const ALARMS: &str = "/api/jobs?kind=backlog-item&metadata_has=overdue_step";

    fn handler(stub: &listing_stub::Stub) -> Arc<JobsAgentStepOverdue> {
        JobsAgentStepOverdue::new(
            stub.base.clone(),
            Arc::new(boss_core::platform_owner::Fixed("emp-owner".into())),
        )
    }

    fn agents_listing() -> Json {
        listing(vec![
            json!({"id": "agent-claude", "aliases": ["claude@algedonic.dev"]}),
        ])
    }

    #[tokio::test]
    async fn a_late_payout_post_files_one_alarm() {
        let stub = listing_stub::serve(vec![
            (AGENTS, agents_listing()),
            (
                OPEN,
                listing(vec![payout(
                    "p1",
                    "2026-09-21T01:10:00Z",
                    "ready",
                    "agent-claude",
                )]),
            ),
            (ALARMS, listing(vec![])),
        ])
        .await;
        handler(&stub).invoke(&args(), &ctx()).await.unwrap();
        let sent = stub.sent();
        assert_eq!(sent.len(), 1, "{sent:?}");
        assert_eq!(sent[0].0, "POST /api/jobs");
        assert_eq!(sent[0].1["metadata"][STEP_KEY], "p1-post");
    }

    /// One alarm per step, ever: an open one is held, and one a person
    /// closed is not re-raised while the step still waits.
    #[tokio::test]
    async fn a_step_already_alarmed_is_not_alarmed_again_in_any_status() {
        for (status, triage) in [("open", "ready"), ("closed", "completed")] {
            let stub = listing_stub::serve(vec![
                (AGENTS, agents_listing()),
                (
                    OPEN,
                    listing(vec![payout(
                        "p1",
                        "2026-09-21T01:10:00Z",
                        "ready",
                        "agent-claude",
                    )]),
                ),
                (ALARMS, listing(vec![alarm_row("p1-post", status, triage)])),
            ])
            .await;
            handler(&stub).invoke(&args(), &ctx()).await.unwrap();
            assert!(stub.writes().is_empty(), "{status}: {:?}", stub.writes());
        }
    }

    #[tokio::test]
    async fn a_step_that_no_longer_waits_withdraws_its_open_alarm() {
        let stub = listing_stub::serve(vec![
            (AGENTS, agents_listing()),
            (OPEN, listing(vec![])),
            (ALARMS, listing(vec![alarm_row("p1-post", "open", "ready")])),
        ])
        .await;
        handler(&stub).invoke(&args(), &ctx()).await.unwrap();
        let sent = stub.sent();
        assert_eq!(sent.len(), 2, "the merge, then the flip: {sent:?}");
        assert_eq!(sent[0].0, "PATCH /api/jobs/al-1/steps/al-1-triage/metadata");
        let m = &sent[0].1;
        assert_eq!(m["disposition"], "stale");
        assert_eq!(m["cleared_by"], HANDLER);
        assert!(
            m.get("authority_role").is_none(),
            "the step's own keys are not re-sent: the merge door keeps them"
        );
        assert_eq!(sent[1].0, "PUT /api/jobs/al-1/steps/al-1-triage");
        assert_eq!(sent[1].1, json!({"status": "completed"}));
    }

    #[tokio::test]
    async fn an_empty_agents_registry_is_refused_not_read_as_no_agents() {
        let stub = listing_stub::serve(vec![
            (AGENTS, listing_stub::empty_listing()),
            (
                OPEN,
                listing(vec![payout(
                    "p1",
                    "2026-09-21T01:10:00Z",
                    "ready",
                    "agent-claude",
                )]),
            ),
            (ALARMS, listing(vec![])),
        ])
        .await;
        let err = handler(&stub).invoke(&args(), &ctx()).await.unwrap_err();
        assert!(
            matches!(err, HandlerError::Permanent(ref m) if m.contains("agents")),
            "{err}"
        );
        assert!(stub.writes().is_empty());
    }

    #[tokio::test]
    async fn an_agents_read_with_no_data_array_refuses_by_name() {
        let stub = listing_stub::serve(vec![(AGENTS, listing_stub::no_data_array())]).await;
        listing_stub::assert_refused_by_name(
            handler(&stub).invoke(&args(), &ctx()).await,
            "GET /api/agents",
        );
        assert!(stub.writes().is_empty());
    }

    #[tokio::test]
    async fn a_daily_firing_without_an_instant_is_a_permanent_refusal() {
        let stub = listing_stub::serve(vec![]).await;
        let mut c = ctx();
        c.event_payload = json!({"_day": "2026-09-23"});
        let err = handler(&stub).invoke(&args(), &c).await.unwrap_err();
        assert!(
            matches!(err, HandlerError::Permanent(ref m) if m.contains("_at") && m.contains("hourly")),
            "{err}"
        );
    }

    /// The shipped rule's declarations parse — every `hours.<workflow>`
    /// is a positive whole number — and name at least the payout, the
    /// case this handler exists for.
    #[test]
    fn the_shipped_rule_declares_the_payout_post() {
        let raw =
            boss_dispatcher::rules::registry::parse_raw_path(boss_testing::dispatcher_rules_dir())
                .expect("parse the shipped rule registry");
        let rule = raw
            .rules
            .iter()
            .find(|r| r.do_steps.iter().any(|d| d.handler == HANDLER))
            .expect("a shipped rule invokes this handler");
        let args: Vec<(String, Value)> = rule
            .do_steps
            .iter()
            .flat_map(|d| d.args.iter())
            .map(|(k, v)| {
                // Each declaration is an integer literal expression.
                let n: i64 = v.trim().trim_matches('"').parse().unwrap_or(-1);
                (k.clone(), Value::Int(n))
            })
            .collect();
        let declared = declarations(&args).unwrap_or_else(|e| panic!("{}: {e}", rule.name));
        assert!(
            declared.contains_key("receive-a-payout"),
            "{}: {declared:?}",
            rule.name
        );
        assert!(
            !declared.contains_key("backlog-item"),
            "backlog builds are the flat list this alarm exists to stand apart from"
        );
    }

    #[test]
    fn the_handler_is_registered_under_its_name() {
        let h = JobsAgentStepOverdue::new(
            "http://unused",
            Arc::new(boss_core::platform_owner::Fixed("emp-owner".into())),
        );
        assert_eq!(h.name(), HANDLER);
        assert!(
            crate::cascade::handler_emits().contains_key(HANDLER),
            "the cascade table knows what this handler emits"
        );
    }
}
