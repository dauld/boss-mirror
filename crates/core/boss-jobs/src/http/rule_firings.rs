//! `GET /api/yard/rule-firings` — every dispatcher rule's newest firing
//! and its dead-letters, for the rules list (backlog 43c4451a, found by
//! page-audit 08a444bc).
//!
//! WHY. `/it/registry/rules` listed each rule's trigger and version and
//! nothing about whether it RUNS, so a stalled `auto-park-on-gate-green`
//! and an idle one painted the same row — the question the world map's
//! borders answer for the one rule they name, asked of every rule.
//!
//! TWO RECORDS THAT ALREADY EXIST, NO NEW ONE. A firing is the
//! `dispatcher_firings` row the rules runner writes when every handler
//! succeeded (b14afc48); a failure past the redelivery budget is the
//! `dead_letter` annotation the runner lands on the packet it owed
//! (a9c498eb). This handler reads both and reduces the second per rule
//! ([`crate::dispatcher_firings::dead_letter_rollup`], pure and
//! unit-tested). It lives beside the borders because it is the same
//! record the borders read; the rules themselves are the dispatcher's,
//! and the page joins the two by rule name.
//!
//! UNREAD IS NOT EMPTY. Each half is `null` with its reason when it
//! cannot be read — no record wired, a failed read, a caller whose
//! policy reads no packets, or more dead-lettered packets than one page
//! holds — because an empty list would paint every rule "no firing" or
//! "no failures" on no evidence, which is how a dead machine reads
//! healthy.

use super::*;

use crate::dispatcher_firings::{
    DEAD_LETTER_KEY, DeadLetterRollup, RETENTION_DAYS, RuleLastFiring, dead_letter_rollup,
};

pub(super) async fn yard_rule_firings<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    CurrentUser(user): CurrentUser,
) -> Response {
    let now = boss_clock_client::now_from(&state.clock).await;
    let since = now - chrono::Duration::days(RETENTION_DAYS);
    let (firings, firings_error) = split(last_firings(&state).await);
    let (dead_letters, dead_letters_error) = split(dead_letters(&state, &user, since).await);
    Json(serde_json::json!({
        "now": now,
        // The window both halves answer: the firing record prunes past
        // it, and the dead-letters are cut to it, so "no firing" means
        // none in this many days and never "never".
        "retention_days": RETENTION_DAYS,
        "firings": firings,
        "firings_error": firings_error,
        "dead_letters": dead_letters,
        "dead_letters_error": dead_letters_error,
    }))
    .into_response()
}

fn split<T>(r: Result<T, String>) -> (Option<T>, Option<String>) {
    match r {
        Ok(v) => (Some(v), None),
        Err(e) => (None, Some(e)),
    }
}

async fn last_firings<R: JobsRepository + 'static, B: EventBus + 'static>(
    state: &Arc<JobsApiState<R, B>>,
) -> Result<Vec<RuleLastFiring>, String> {
    let repo = state
        .dispatcher_firings
        .as_ref()
        .ok_or("the dispatcher firing record is not wired to this jobs API")?;
    repo.last_firings()
        .await
        .map_err(|e| format!("reading dispatcher_firings: {e}"))
}

/// The dead-letter annotations the caller can see, rolled up per rule.
/// Scoped by the caller's read policy like every packet read here; a
/// caller the policy grants no packets is told the count is unknown,
/// not that it is zero.
async fn dead_letters<R: JobsRepository + 'static, B: EventBus + 'static>(
    state: &Arc<JobsApiState<R, B>>,
    user: &boss_policy_client::User,
    since: chrono::DateTime<chrono::Utc>,
) -> Result<Vec<DeadLetterRollup>, String> {
    let predicate = state
        .policy
        .scope_predicate(user, Resource::job())
        .await
        .map_err(|e| format!("policy check failed: {e}"))?;
    if matches!(predicate, boss_policy_client::Predicate::None) {
        return Err(
            "this caller's policy scope reads no packets, so the dead-letters \
                    on them cannot be counted"
                .to_string(),
        );
    }
    let filter = JobFilter {
        metadata_has: Some(DEAD_LETTER_KEY.to_string()),
        scope: job_scope_from_predicate(user, &predicate),
        ..Default::default()
    };
    let (rows, total) = state
        .jobs
        .list_jobs(&filter, MAX_LIMIT, 0)
        .await
        .map_err(|e| format!("reading dead-lettered packets: {e}"))?;
    // A limit is not a filter: a page short of the total would count a
    // sample and call it the whole. Refused, never truncated.
    if total > rows.len() as i64 {
        return Err(format!(
            "{total} packets carry a dead-letter and one read holds {}; a per-rule count \
             of part of them would undercount",
            rows.len()
        ));
    }
    Ok(dead_letter_rollup(&rows, since))
}
