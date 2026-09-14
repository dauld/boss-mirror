//! Every seeded rule must only reference fields its event declares.
//!
//! This is the gate cf7ae3b5 asked for: "turn today's dead-letter into a
//! 422 at authoring time". It runs against the SEEDED registry —
//! `infra/dispatcher/rules/` published into `dispatcher_rules` the way
//! the dispatcher's boot step does it, over the `event_kinds` rows a
//! deployment gets — so a rule file binding a field its topic does not
//! carry fails here rather than eight NAKs into production. Until
//! 2026-09-14 this read the table alone, which since the one-file-per-
//! rule collapse (41ba00cd) holds the last MIGRATED version of every
//! rule, not the file the tree ships (backlog 488cadca): a v4 binding a
//! new field would have passed here unread.
//!
//! It is a ratchet. A topic whose `payload_fields` roster is empty is not
//! checked, so this starts covering `jobs.job.closed` (migration 137, the
//! most-bound topic and the one that actually dead-lettered) and widens
//! one roster at a time. The second test below is what makes that honest:
//! it asserts at least one roster exists, so the whole file cannot quietly
//! degrade into a no-op if a migration ever empties the column.

use std::collections::{BTreeMap, BTreeSet};

use boss_dispatcher::rules::payload_contract::unresolved_identifiers;
use boss_dispatcher::rules::registry::Rule;
use boss_testing::TestDb;

mod common;

/// kind_pattern → declared field names, for kinds that declare any.
async fn rosters(pool: &sqlx::PgPool) -> BTreeMap<String, BTreeSet<String>> {
    let rows: Vec<(String, serde_json::Value)> =
        sqlx::query_as("SELECT kind_pattern, payload_fields FROM event_kinds")
            .fetch_all(pool)
            .await
            .expect("read event_kinds");
    rows.into_iter()
        .map(|(kind, fields)| {
            let names: BTreeSet<String> = fields
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|f| f.get("name")?.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            (kind, names)
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn seeded_rules_only_reference_fields_their_event_declares() {
    let db = TestDb::new().await;
    let rosters = rosters(&db.pool).await;
    let raw = common::shipped_raw_rules(&db).await;

    let mut failures: Vec<String> = Vec::new();
    for raw_rule in raw.rules {
        // Only event-triggered rules bind a payload; a scheduled rule has
        // no event to check against.
        let Some(topic) = raw_rule.on_event.clone() else {
            continue;
        };
        // An exact roster only. A pattern topic (`step.done.*`) has one
        // roster per concrete suffix, which no kind declares yet — and
        // guessing which row covers a wildcard is how a gate earns a
        // false positive.
        let Some(roster) = rosters.get(&topic) else {
            continue;
        };
        let name = raw_rule.name.clone();
        let rule = match Rule::from_raw(raw_rule) {
            Ok(r) => r,
            // Parse failures are a different gate's business.
            Err(_) => continue,
        };
        for bad in unresolved_identifiers(&rule, roster) {
            failures.push(format!(
                "rule `{name}` on `{topic}`: {bad} — `{topic}` declares [{}]",
                roster.iter().cloned().collect::<Vec<_>>().join(", ")
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "a rule binds a field its event does not carry. At runtime this is \
         not a quiet false — the evaluator returns UnknownIdentifier, the \
         runner NAKs, and the event dead-letters after eight attempts \
         (cf7ae3b5). Either add the field to every emit site of the topic, \
         or stop binding it:\n  {}",
        failures.join("\n  ")
    );
}

/// The ratchet must actually be engaged.
///
/// `unresolved_identifiers` returns nothing for a kind with an empty
/// roster, which is what lets this land incrementally — and would also
/// let the test above pass while checking absolutely nothing. If a
/// migration ever empties `payload_fields`, this fails instead of the
/// coverage silently going to zero.
#[tokio::test(flavor = "multi_thread")]
async fn at_least_one_event_kind_declares_its_payload() {
    let db = TestDb::new().await;
    let declared: Vec<String> = rosters(&db.pool)
        .await
        .into_iter()
        .filter(|(_, fields)| !fields.is_empty())
        .map(|(kind, _)| kind)
        .collect();

    assert!(
        !declared.is_empty(),
        "no event kind declares payload_fields, so the rule/payload gate \
         above checks nothing. Migration 137 seeds jobs.job.closed; if it \
         was reverted, revert this ratchet deliberately rather than by \
         accident."
    );
    assert!(
        declared.iter().any(|k| k == "jobs.job.closed"),
        "jobs.job.closed lost its roster — it is the most-bound topic and \
         the one that dead-lettered; declared: {declared:?}"
    );
}
