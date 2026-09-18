//! `retro.open` — the week's retros, one per department the classes
//! registry holds, plus the platform's own (design 3613f0af, backlog
//! 1dffde5d).
//!
//! WHY A HANDLER AND NOT SEVEN `jobs.spawn` RULES. `jobs.spawn` opens
//! exactly one packet per invocation from literal args, and a rule's
//! `when` cannot iterate; so "one department-retro per department" as
//! rule data alone is one file PER DEPARTMENT, each naming a code the
//! classes registry already holds — a list living twice (CLAUDE.md
//! §9a), and a department declared on Tuesday with no retro until
//! someone lands a file. This handler reads the list at fire time from
//! `GET /api/departments` (the jobs API's classes-backed read), so the
//! rule stays ONE file and the registry stays the one definition.
//!
//! THE DEDUP IS A WEEK WINDOW, NOT `open_job_exists`. A clock rule
//! guarded by an open packet stops firing FOREVER once one packet is
//! left open, and says nothing — measured 2026-09-10, nineteen days of
//! no mirror check behind one undrained publish packet
//! (a-dedup-guard-silently-retires-a-cadence). A retro left open at
//! `review` for a fortnight must not cancel every later week's. So the
//! question asked per department is "was one opened THIS ISO WEEK?" —
//! the newest packet's `opened_on` against the firing day — which
//! makes the firing idempotent (a NAK'd retry re-asks and re-skips), a
//! catch-up burst after an outage collapses to one packet, and a
//! packet from last week suppresses nothing.
//!
//! THE PLATFORM'S RETRO RIDES THE SAME FIRING. `protocol-retro` was a
//! `cadence_rules` row (`protocol-retro-daily`, verb `open:<kind>`,
//! migration 202608282140) because on 2026-08-28 the dispatcher could
//! not yet schedule a packet; it can, and this is the sanctioned shape.
//! The rule's `platform_kind` / `platform_subject` args name it, the
//! same week window applies, and the packet keeps the subject the
//! cadence loop's `packet_body` filed under (`infra/protocol-retro`) so
//! the two mechanisms cannot file twins while the row is still active.
//! What this retires: the cadence row (an operator act — there is no
//! write door on `/api/cadence/rules`) and, after it, the `open:` verb
//! in boss-cli's cadence loop, which then has no user.

use std::sync::Arc;

use async_trait::async_trait;
use boss_dispatcher::rules::expr::Value as ExprValue;
use boss_dispatcher::rules::handler::{Handler, HandlerError, InvocationContext, arg, arg_string};
use chrono::{Datelike, NaiveDate};
use serde_json::{Value, json};

use super::common::{api_client, get_json, owner_for_filing, post_json};

pub struct RetroOpen {
    client: reqwest::Client,
    jobs_base: String,
    /// The firing day every window is judged against — the clock
    /// service, like every other stamp the dispatcher makes.
    clock: Arc<dyn boss_clock_client::ClockClient>,
    /// Who the retros are filed to: the platform owner through the port
    /// (backlog 3c23662d), never a literal person.
    owner: Arc<dyn boss_core::platform_owner::PlatformOwner>,
}

impl RetroOpen {
    pub fn new(
        jobs_base: impl Into<String>,
        clock_url: impl Into<String>,
        owner: Arc<dyn boss_core::platform_owner::PlatformOwner>,
    ) -> Arc<Self> {
        Arc::new(Self {
            client: api_client(),
            jobs_base: jobs_base.into(),
            clock: Arc::new(boss_clock_client::ReqwestClockClient::new(clock_url)),
            owner,
        })
    }

    fn base(&self) -> &str {
        self.jobs_base.trim_end_matches('/')
    }
}

/// Two days in the same ISO week (Monday-anchored, the week the
/// `department-retros-weekly` rule fires on).
pub fn same_iso_week(a: NaiveDate, b: NaiveDate) -> bool {
    a.iso_week() == b.iso_week()
}

/// `YYYY-Www`, the week a retro covers, stamped on the packet.
pub fn week_label(day: NaiveDate) -> String {
    let w = day.iso_week();
    format!("{}-W{:02}", w.year(), w.week())
}

/// What one retro's firing decides, from the newest packet's opened
/// day alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Open,
    /// A packet already opened this week: its opened day.
    AlreadyThisWeek(NaiveDate),
}

/// THE dedup, pure: open unless the newest packet was opened in the
/// firing day's ISO week. `None` (no packet ever) opens.
pub fn decide(newest_opened_on: Option<NaiveDate>, today: NaiveDate) -> Decision {
    match newest_opened_on {
        Some(d) if same_iso_week(d, today) => Decision::AlreadyThisWeek(d),
        _ => Decision::Open,
    }
}

/// The newest packet's `opened_on` out of a `/api/jobs?…&limit=1`
/// listing (newest-opened first), or `None` for an empty page.
pub fn newest_opened_on(listing: &Value) -> Option<NaiveDate> {
    listing
        .get("data")?
        .as_array()?
        .first()?
        .get("opened_on")?
        .as_str()
        .and_then(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok())
}

/// One department's retro packet. The Subject is the department code
/// (the `(kind, subject)` identity the silence sweep and this dedup
/// read) and `metadata.department` carries the same code — the field
/// the kind's admission contract requires and the readiness read
/// narrows on. Both are written in the one POST, so they cannot drift.
pub fn department_packet(
    kind: &str,
    code: &str,
    display_name: &str,
    today: NaiveDate,
    owner: &str,
    ctx: &InvocationContext,
) -> Value {
    json!({
        "kind": kind,
        "subject": { "subject_kind": "custom", "id": code },
        "title": format!("{display_name} retro — week {}", week_label(today)),
        "owner_id": owner,
        "priority": "standard",
        "status": "open",
        "metadata": {
            "department": code,
            "week": week_label(today),
            "spawned_by_rule": ctx.rule_name,
            "triggered_by_event_id": ctx.triggering_event_id,
            "triggered_by_topic": ctx.triggering_topic,
        },
        "tags": ["dispatcher-spawned"],
    })
}

/// The platform's own retro, under the subject the cadence loop's
/// `packet_body` already files it under so the two cannot twin.
pub fn platform_packet(
    kind: &str,
    subject: &str,
    today: NaiveDate,
    owner: &str,
    ctx: &InvocationContext,
) -> Value {
    json!({
        "kind": kind,
        "subject": { "subject_kind": "custom", "id": subject },
        "title": format!("{kind} — week {}", week_label(today)),
        "owner_id": owner,
        "priority": "standard",
        "status": "open",
        "metadata": {
            "week": week_label(today),
            "spawned_by_rule": ctx.rule_name,
            "triggered_by_event_id": ctx.triggering_event_id,
            "triggered_by_topic": ctx.triggering_topic,
        },
        "tags": ["dispatcher-spawned"],
    })
}

/// One department as `GET /api/departments` lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Department {
    pub code: String,
    pub display_name: String,
}

/// The department rows out of the listing; a row without a code is
/// skipped by name in the error list, never silently.
pub fn departments(listing: &Value) -> Result<Vec<Department>, String> {
    let rows = listing
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| "GET /api/departments answered no `data` array".to_string())?;
    rows.iter()
        .map(|r| {
            let code = r
                .get("code")
                .and_then(Value::as_str)
                .ok_or_else(|| format!("a department row carries no code: {r}"))?;
            Ok(Department {
                code: code.to_string(),
                display_name: r
                    .get("display_name")
                    .and_then(Value::as_str)
                    .unwrap_or(code)
                    .to_string(),
            })
        })
        .collect()
}

/// An optional string arg, refused when present and not a string.
fn optional_string(
    args: &[(String, ExprValue)],
    name: &str,
) -> Result<Option<String>, HandlerError> {
    match arg(args, name) {
        None => Ok(None),
        Some(ExprValue::String(s)) if !s.is_empty() => Ok(Some(s.clone())),
        Some(ExprValue::String(_)) => Ok(None),
        Some(other) => Err(HandlerError::BadArgType {
            arg: name.to_string(),
            expected: "string",
            got: other.kind(),
        }),
    }
}

#[async_trait]
impl Handler for RetroOpen {
    fn name(&self) -> &'static str {
        "retro.open"
    }

    async fn invoke(
        &self,
        args: &[(String, ExprValue)],
        ctx: &InvocationContext,
    ) -> Result<(), HandlerError> {
        let department_kind = arg_string(args, "department_kind")?;
        let platform_kind = optional_string(args, "platform_kind")?;
        let platform_subject = optional_string(args, "platform_subject")?;
        if platform_kind.is_some() != platform_subject.is_some() {
            return Err(HandlerError::Permanent(
                "retro.open: `platform_kind` and `platform_subject` are declared together or \
                 not at all — the subject is the identity the dedup and the silence sweep read"
                    .into(),
            ));
        }
        let today = boss_clock_client::now_from(&self.clock).await.date_naive();
        let owner = owner_for_filing(self.owner.as_ref(), &ctx.rule_name).await;

        // The list is read, never declared: a department the registry
        // holds tonight has its retro on Monday with no edit anywhere.
        let listing = get_json(
            &self.client,
            &format!("{}/api/departments", self.base()),
            &ctx.rule_name,
        )
        .await?;
        let departments = departments(&listing).map_err(HandlerError::Downstream)?;
        if departments.is_empty() {
            tracing::warn!(
                rule = %ctx.rule_name,
                "retro.open: the classes registry holds no department — nothing to review"
            );
        }

        // Best-effort accumulator, the sweep's posture: one department's
        // failed read or POST must never cost the others their retro.
        // Every failure surfaces as ONE error at the end so the firing
        // NAKs for redelivery, where the week window makes the retry
        // idempotent.
        let mut errors: Vec<String> = Vec::new();
        let mut opened = 0usize;
        for d in &departments {
            let path = format!(
                "/api/jobs?kind={department_kind}&subject_id={}&limit=1",
                d.code
            );
            let newest = match get_json(
                &self.client,
                &format!("{}{path}", self.base()),
                &ctx.rule_name,
            )
            .await
            {
                Ok(l) => newest_opened_on(&l),
                Err(e) => {
                    errors.push(format!("{}: newest-retro read failed: {e}", d.code));
                    continue;
                }
            };
            match decide(newest, today) {
                Decision::AlreadyThisWeek(day) => {
                    tracing::info!(
                        department = %d.code,
                        opened_on = %day,
                        "retro.open: a retro was opened this week already — not filing a twin"
                    );
                }
                Decision::Open => {
                    let body = department_packet(
                        department_kind,
                        &d.code,
                        &d.display_name,
                        today,
                        &owner,
                        ctx,
                    );
                    match post_json(
                        &self.client,
                        &format!("{}/api/jobs", self.base()),
                        &body,
                        &ctx.rule_name,
                    )
                    .await
                    {
                        Ok(()) => {
                            opened += 1;
                            tracing::info!(department = %d.code, "retro.open: filed a {department_kind}");
                        }
                        Err(e) => errors.push(format!("{}: filing failed: {e}", d.code)),
                    }
                }
            }
        }

        if let (Some(kind), Some(subject)) = (platform_kind, platform_subject) {
            let path = format!("/api/jobs?kind={kind}&subject_id={subject}&limit=1");
            match get_json(
                &self.client,
                &format!("{}{path}", self.base()),
                &ctx.rule_name,
            )
            .await
            {
                Err(e) => errors.push(format!("{kind}: newest-retro read failed: {e}")),
                Ok(l) => match decide(newest_opened_on(&l), today) {
                    Decision::AlreadyThisWeek(day) => {
                        tracing::info!(kind = %kind, opened_on = %day, "retro.open: the platform retro was opened this week already");
                    }
                    Decision::Open => {
                        let body = platform_packet(&kind, &subject, today, &owner, ctx);
                        match post_json(
                            &self.client,
                            &format!("{}/api/jobs", self.base()),
                            &body,
                            &ctx.rule_name,
                        )
                        .await
                        {
                            Ok(()) => {
                                opened += 1;
                                tracing::info!(kind = %kind, "retro.open: filed the platform retro");
                            }
                            Err(e) => errors.push(format!("{kind}: filing failed: {e}")),
                        }
                    }
                },
            }
        }

        tracing::info!(
            rule = %ctx.rule_name,
            week = %week_label(today),
            departments = departments.len(),
            opened,
            failed = errors.len(),
            "retro.open pass complete"
        );
        if errors.is_empty() {
            Ok(())
        } else {
            Err(HandlerError::Downstream(format!(
                "retro.open: {} sub-operation(s) failed this pass (every retro that could open \
                 did; the rest retry): {}",
                errors.len(),
                errors.join(" | ")
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").expect("a date")
    }

    fn ctx() -> InvocationContext {
        InvocationContext {
            rule_name: "department-retros-weekly".into(),
            triggering_event_id: "clock-day:2026-09-21".into(),
            triggering_topic: "clock.day".into(),
            event_payload: json!({ "_day": "2026-09-21" }),
        }
    }

    /// The rule fires Monday 00:00Z; last Monday's packet is in the
    /// previous ISO week however close to midnight either was opened,
    /// so the firing opens. That is the jitter a "7 days ago" window
    /// gets wrong at the boundary.
    #[test]
    fn last_mondays_retro_does_not_suppress_this_mondays() {
        let monday = d("2026-09-21");
        assert_eq!(decide(Some(d("2026-09-14")), monday), Decision::Open);
        assert_eq!(
            decide(Some(d("2026-09-20")), monday),
            Decision::Open,
            "Sunday is last week"
        );
        assert_eq!(decide(None, monday), Decision::Open, "never opened");
    }

    /// A packet opened this week — by a retry, a catch-up firing, or a
    /// hand — suppresses, whatever its status. An OPEN packet from an
    /// earlier week does not: that is the guard that retired cadences
    /// for nineteen days (a-dedup-guard-silently-retires-a-cadence).
    #[test]
    fn this_weeks_retro_suppresses_and_an_older_open_one_does_not() {
        let wednesday = d("2026-09-23");
        assert_eq!(
            decide(Some(d("2026-09-21")), wednesday),
            Decision::AlreadyThisWeek(d("2026-09-21"))
        );
        assert_eq!(
            decide(Some(d("2026-09-27")), d("2026-09-21")),
            Decision::AlreadyThisWeek(d("2026-09-27")),
            "Sunday closes the week"
        );
        assert_eq!(
            decide(Some(d("2026-09-07")), wednesday),
            Decision::Open,
            "a fortnight-old packet, open or not, suppresses nothing"
        );
    }

    #[test]
    fn the_week_label_is_the_iso_week() {
        assert_eq!(week_label(d("2026-09-21")), "2026-W39");
        assert_eq!(week_label(d("2026-01-01")), "2026-W01");
        // ISO: 2027-01-01 is a Friday in 2026's last week.
        assert_eq!(week_label(d("2027-01-01")), "2026-W53");
    }

    #[test]
    fn the_newest_packet_is_the_first_row_of_the_listing() {
        let l = json!({ "data": [
            { "id": "a", "opened_on": "2026-09-21", "status": "open" },
            { "id": "b", "opened_on": "2026-09-14", "status": "closed" },
        ], "total": 2 });
        assert_eq!(newest_opened_on(&l), Some(d("2026-09-21")));
        assert_eq!(newest_opened_on(&json!({ "data": [], "total": 0 })), None);
        assert_eq!(newest_opened_on(&json!({ "error": "x" })), None);
    }

    #[test]
    fn the_department_list_is_read_by_code() {
        let l = json!({ "data": [
            { "code": "sales", "display_name": "Sales" },
            { "code": "support" },
        ], "total": 2 });
        assert_eq!(
            departments(&l).expect("rows"),
            vec![
                Department {
                    code: "sales".into(),
                    display_name: "Sales".into()
                },
                Department {
                    code: "support".into(),
                    display_name: "support".into()
                },
            ]
        );
        assert!(departments(&json!({ "data": [{ "display_name": "x" }] })).is_err());
        assert!(
            departments(&json!({ "total": 0 })).is_err(),
            "no data array is a refusal, not an empty list"
        );
    }

    /// The department packet carries the code TWICE by design — as the
    /// Subject (the `(kind, subject)` identity the dedup and the silence
    /// sweep read) and as `metadata.department` (the admission contract
    /// and the readiness read's key) — in one write.
    #[test]
    fn a_department_packet_names_its_department_as_subject_and_metadata() {
        let b = department_packet(
            "department-retro",
            "sales",
            "Sales",
            d("2026-09-21"),
            "emp-owner",
            &ctx(),
        );
        assert_eq!(b["kind"], "department-retro");
        assert_eq!(b["subject"]["id"], "sales");
        assert_eq!(b["subject"]["subject_kind"], "custom");
        assert_eq!(b["metadata"]["department"], "sales");
        assert_eq!(b["metadata"]["week"], "2026-W39");
        assert_eq!(b["metadata"]["spawned_by_rule"], "department-retros-weekly");
        assert_eq!(b["owner_id"], "emp-owner");
        assert_eq!(b["title"], "Sales retro — week 2026-W39");
    }

    /// The platform retro keeps the subject boss-cli's cadence loop
    /// files under (`infra/protocol-retro`), so while the old row is
    /// still active the loop's single-open check sees this packet.
    #[test]
    fn the_platform_packet_keeps_the_cadence_loops_subject() {
        let b = platform_packet(
            "protocol-retro",
            "infra/protocol-retro",
            d("2026-09-21"),
            "emp-owner",
            &ctx(),
        );
        assert_eq!(b["kind"], "protocol-retro");
        assert_eq!(b["subject"]["id"], "infra/protocol-retro");
        assert!(
            b["metadata"].get("department").is_none(),
            "the platform is not a department"
        );
        assert_eq!(b["metadata"]["week"], "2026-W39");
    }
}
