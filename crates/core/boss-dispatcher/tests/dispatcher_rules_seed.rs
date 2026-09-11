//! The authored directory IS the dispatcher-rule registry's definition;
//! the `dispatcher_rules` table is DERIVED from it.
//!
//! WHAT THESE TESTS REPLACE. Until this file changed,
//! `dispatcher_rules_seed_matches_toml` compared `infra/dispatcher/rules/`
//! against the SQL seed migrations in both directions — a §9a PIN, which
//! CLAUDE.md is explicit is a holding action and not a destination. It
//! detected drift between two copies of one fact and left both copies
//! there, so every rule change still had to be written twice, in two
//! languages: a TOML file and an `INSERT INTO dispatcher_rules`. The pin
//! was deleted deliberately rather than kept, because after the collapse
//! it would fail the very workflow it is supposed to protect: the tree
//! LEADING the migrations (a rule whose file exists and whose row no
//! migration writes) is now the correct, intended state, not drift.
//!
//! What replaces it is strictly stronger. Each test below MUTATES the
//! derived home — deletes a row, adds a file the migrations never
//! mention, bumps a version, removes a file — and then asserts the
//! system answers from the definition. A test that must first break the
//! derived copy cannot pass by tautology (backlog 024c0db2 is about
//! exactly that failure mode), and `the_registry_equals_the_authored_
//! directory_after_a_seed` still carries the old pin's two-direction
//! equality as its final assertion.
//!
//! Postgres-only: TestDb applies the full schema (incl 41-dispatcher.sql
//! and every `NNN-dispatcher-rule-*.sql`), so every test starts from the
//! registry a fresh deployment gets from migrations alone.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use boss_dispatcher::rules::registry::{load_active_rules, parse_raw_path};
use boss_dispatcher::rules::seed::seed_authored_rules;
use boss_testing::TestDb;

/// The authored registry: the directory, not a file. Adding a rule is
/// dropping a file in (CLAUDE.md §9a — the collapse `infra/postgres/schema/`
/// and `infra/platform/workflows/` already had).
const RULES_DIR: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../infra/dispatcher/rules"
);

/// The migration directory, read to make "no second edit" a CHECKED
/// claim rather than a narrated one.
const SCHEMA_DIR: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../infra/postgres/schema"
);

/// `name -> serialized rule`, the comparison the old pin used: arg maps
/// and field order do not matter, the JSON compares structurally.
fn by_name(
    raw: &boss_dispatcher::rules::registry::RawRegistry,
) -> BTreeMap<String, serde_json::Value> {
    raw.rules
        .iter()
        .map(|r| (r.name.clone(), serde_json::to_value(r).expect("serialize")))
        .collect()
}

/// A throwaway copy of the authored directory, so a test can add,
/// change or remove a rule file without touching the tree.
fn authored_copy(tmp: &tempfile::TempDir) -> PathBuf {
    let dir = tmp.path().join("rules");
    std::fs::create_dir_all(&dir).expect("create the fixture registry");
    for entry in std::fs::read_dir(RULES_DIR).expect("read the authored registry") {
        let src = entry.expect("dir entry").path();
        if src.extension().and_then(|e| e.to_str()) != Some("toml") {
            continue;
        }
        let dst = dir.join(src.file_name().expect("file name"));
        std::fs::copy(&src, &dst).expect("copy a rule file");
    }
    dir
}

/// A rule file's text with its `version` set to `to`. `version` belongs
/// to the `[[rule]]` table, which the `[[rule.do]]` sub-table ends, so
/// the key is replaced in place (or inserted right after `name`) rather
/// than appended at the end of the file.
fn bump_version(file: &Path, to: u32) -> String {
    let src = std::fs::read_to_string(file).expect("read the rule file");
    let mut out = String::with_capacity(src.len() + 16);
    let mut written = false;
    for line in src.lines() {
        if line.starts_with("version = ") {
            out.push_str(&format!("version = {to}\n"));
            written = true;
            continue;
        }
        out.push_str(line);
        out.push('\n');
        if !written && line.starts_with("name = ") {
            out.push_str(&format!("version = {to}\n"));
            written = true;
        }
    }
    assert!(
        written,
        "the rule file has no `name` line: {}",
        file.display()
    );
    out
}

/// Does any migration in the tree mention `name`? The question behind
/// "a rule lands by dropping a file in, and nothing else".
fn a_migration_mentions(name: &str) -> bool {
    std::fs::read_dir(SCHEMA_DIR)
        .expect("read the schema directory")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("sql"))
        .any(|p| {
            std::fs::read_to_string(&p)
                .map(|s| s.contains(name))
                .unwrap_or(false)
        })
}

async fn active_version(pool: &sqlx::PgPool, name: &str) -> Option<i32> {
    sqlx::query_scalar("SELECT version FROM dispatcher_rules WHERE name = $1 AND status = 'active'")
        .bind(name)
        .fetch_optional(pool)
        .await
        .expect("query the active version")
}

async fn status_of(pool: &sqlx::PgPool, name: &str, version: i32) -> Option<String> {
    sqlx::query_scalar("SELECT status FROM dispatcher_rules WHERE name = $1 AND version = $2")
        .bind(name)
        .bind(version)
        .fetch_optional(pool)
        .await
        .expect("query a version's status")
}

/// THE COLLAPSE, in the direction that proves it is one: break the
/// DERIVED home and show the system still answers from the definition.
///
/// Three rules are deleted outright — every version of each — and the
/// seed puts them back from the directory, at the version the file
/// declares (one of the three is at v2, so this also pins that the seed
/// honours the authored version rather than assigning `MAX+1`).
///
/// The final assertion is the OLD PIN's property, now as an outcome of
/// the derivation rather than a comparison between two maintained
/// copies: the enforced registry equals the authored directory, in both
/// directions, so a migration that seeded a rule no file records shows
/// up here as an extra live rule.
#[tokio::test(flavor = "multi_thread")]
async fn the_registry_equals_the_authored_directory_after_a_seed() {
    let db = TestDb::new().await;
    let gone = [
        "converge-on-merge",
        "cadence-silence-sweep-daily",
        "complete-marker-on-step-ready",
    ];
    for name in gone {
        sqlx::query("DELETE FROM dispatcher_rules WHERE name = $1")
            .bind(name)
            .execute(&db.pool)
            .await
            .expect("delete a rule from the derived registry");
        assert!(
            active_version(&db.pool, name).await.is_none(),
            "{name} must be gone from the derived registry before the seed runs, \
             or this test proves nothing"
        );
    }

    let report = seed_authored_rules(&db.pool, Path::new(RULES_DIR))
        .await
        .expect("seed the authored registry");

    for name in gone {
        assert!(
            report.inserted.iter().any(|(n, _)| n == name),
            "the seed must report restoring {name}; it reported {:?}",
            report.inserted
        );
    }
    let from_toml = parse_raw_path(RULES_DIR).expect("parse the rule directory");
    let authored_version = from_toml
        .rules
        .iter()
        .find(|r| r.name == "cadence-silence-sweep-daily")
        .map(|r| r.version)
        .expect("the silence sweep is authored");
    assert_eq!(
        active_version(&db.pool, "cadence-silence-sweep-daily").await,
        Some(authored_version as i32),
        "a restored rule takes the version its FILE declares, not MAX+1"
    );

    let from_db = load_active_rules(&db.pool)
        .await
        .expect("load active rules from dispatcher_rules");
    assert_eq!(
        by_name(&from_db),
        by_name(&from_toml),
        "after the seed, the registry the dispatcher enforces must BE the authored \
         directory — every file live, and no live rule the directory does not author"
    );
}

/// The other half of the collapse: a rule that exists ONLY as a file
/// reaches the registry, with no migration anywhere in the tree.
///
/// `a_migration_mentions` is checked, not narrated — the claim under
/// test is "a rule lands by dropping a file in", and a test that
/// quietly depended on a migration existing would be measuring the old
/// world.
#[tokio::test(flavor = "multi_thread")]
async fn a_rule_file_alone_reaches_the_registry() {
    let db = TestDb::new().await;
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = authored_copy(&tmp);

    let name = "zzz-a-rule-no-migration-ever-wrote";
    std::fs::write(
        dir.join(format!("{name}.toml")),
        format!(
            r#"
[[rule]]
name = "{name}"
why = """
A TEST FIXTURE, and the one rule in this registry whose whole point is
that no migration mentions it.
"""
on_event = "step.done.task"
when = 'spec_slug = "a-slug-no-step-has"'
[[rule.do]]
handler = "webhook.notify"
"#
        ),
    )
    .expect("write the new rule file");

    assert!(
        !a_migration_mentions(name),
        "the fixture rule must be absent from every migration, or this test \
         is not measuring the one-home claim"
    );
    assert!(
        active_version(&db.pool, name).await.is_none(),
        "the fixture rule must not be in the derived registry before the seed"
    );

    let report = seed_authored_rules(&db.pool, &dir)
        .await
        .expect("seed the fixture registry");

    assert_eq!(
        active_version(&db.pool, name).await,
        Some(1),
        "a rule authored only as a file must become active at v1 — reported: {report:?}"
    );
    assert!(
        report.retired.is_empty(),
        "seeding a superset of the authored directory must retire nothing: {:?}",
        report.retired
    );
    let live = load_active_rules(&db.pool)
        .await
        .expect("load active rules");
    let got = live
        .rules
        .iter()
        .find(|r| r.name == name)
        .expect("the new rule is enforced");
    assert_eq!(got.on_event.as_deref(), Some("step.done.task"));
    assert_eq!(got.do_steps.len(), 1);
    assert_eq!(got.do_steps[0].handler, "webhook.notify");
}

/// Changing a rule is bumping `version` in its file. The seed appends
/// the new version and retires the incumbent — the append-only,
/// versioned contract the table already has, driven from the tree
/// instead of from a second `INSERT`.
#[tokio::test(flavor = "multi_thread")]
async fn a_version_bump_in_the_tree_supersedes_the_live_row() {
    let db = TestDb::new().await;
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = authored_copy(&tmp);

    let name = "converge-on-merge";
    let before = active_version(&db.pool, name)
        .await
        .expect("the migrations seeded it");
    let file = dir.join(format!("{name}.toml"));
    std::fs::write(&file, bump_version(&file, before as u32 + 1))
        .expect("write the bumped rule file");

    let report = seed_authored_rules(&db.pool, &dir)
        .await
        .expect("seed the fixture registry");

    assert_eq!(
        active_version(&db.pool, name).await,
        Some(before + 1),
        "the bumped version must be the enforced one — reported: {report:?}"
    );
    assert_eq!(
        status_of(&db.pool, name, before).await.as_deref(),
        Some("retired"),
        "the superseded version must be retired, not deleted (append-only)"
    );
}

/// The seed never walks a version BACK. An operator publishing live
/// through `POST /api/dispatcher/rules` is still supported — that is
/// what "registry data, editable without a deploy" means — so a live
/// version ahead of the tree is reported, loudly, and left alone. The
/// drift-healing posture that republishes the tree over it is the one
/// that silently undid two protocol edits on 2026-08-14 (68331085).
#[tokio::test(flavor = "multi_thread")]
async fn the_seed_never_walks_back_a_version_the_registry_already_has() {
    let db = TestDb::new().await;
    let name = "complete-marker-on-step-ready";
    let authored = active_version(&db.pool, name)
        .await
        .expect("the migrations seeded it");
    let ahead = authored + 4;
    sqlx::query(
        "UPDATE dispatcher_rules SET status = 'retired' WHERE name = $1 AND status = 'active'",
    )
    .bind(name)
    .execute(&db.pool)
    .await
    .expect("retire the incumbent");
    sqlx::query(
        "INSERT INTO dispatcher_rules (name, version, status, on_event, when_expr, do_steps) \
         VALUES ($1, $2, 'active', 'step.ready.*', NULL, \
                 '[{\"handler\":\"jobs.complete_step\",\"args\":{}}]'::jsonb)",
    )
    .bind(name)
    .bind(ahead)
    .execute(&db.pool)
    .await
    .expect("author a newer version live");

    let report = seed_authored_rules(&db.pool, Path::new(RULES_DIR))
        .await
        .expect("seed the authored registry");

    assert_eq!(
        active_version(&db.pool, name).await,
        Some(ahead),
        "an operator's newer live version must survive the seed"
    );
    assert!(
        report.behind.iter().any(|(n, _, _)| n == name),
        "the seed must SAY the tree is behind for {name}: {:?}",
        report.behind
    );
}

/// Retiring a rule is deleting its file. Without this the tree would be
/// the definition for adding and changing a rule but NOT for removing
/// one, which leaves retirement needing a migration — two homes for one
/// operation, the half-collapse §9a warns is worse than the pin.
#[tokio::test(flavor = "multi_thread")]
async fn a_rule_the_tree_no_longer_authors_is_retired() {
    let db = TestDb::new().await;
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = authored_copy(&tmp);

    let name = "forward-handoff-done-to-webhook";
    assert!(
        active_version(&db.pool, name).await.is_some(),
        "the migrations seeded it, so there is something to retire"
    );
    std::fs::remove_file(dir.join(format!("{name}.toml"))).expect("delete the rule file");

    let report = seed_authored_rules(&db.pool, &dir)
        .await
        .expect("seed the fixture registry");

    assert_eq!(
        active_version(&db.pool, name).await,
        None,
        "a rule no file authors must stop being enforced — reported: {report:?}"
    );
    assert!(
        report.retired.iter().any(|n| n == name),
        "the seed must NAME what it retired: {:?}",
        report.retired
    );
}

/// An unreadable authored registry writes NOTHING. The directory comes
/// from `BOSS_DISPATCHER_RULES`, and a wrong target that answers instead
/// of erroring is the §Doors mistake; here it would read as "the tree
/// authors no rules" and retire the entire registry.
#[tokio::test(flavor = "multi_thread")]
async fn an_unreadable_authored_registry_writes_nothing() {
    let db = TestDb::new().await;
    let before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM dispatcher_rules")
        .fetch_one(&db.pool)
        .await
        .expect("count rules");
    assert!(before > 0, "the migrations seeded rules to protect");

    let err = seed_authored_rules(&db.pool, Path::new("/nonexistent/dispatcher/rules"))
        .await
        .expect_err("an absent registry directory must be an error, never an empty registry");
    assert!(
        err.to_string().contains("dispatcher") || err.to_string().contains("nonexistent"),
        "the error names the path it could not read: {err}"
    );

    let after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM dispatcher_rules")
        .fetch_one(&db.pool)
        .await
        .expect("count rules");
    assert_eq!(before, after, "a failed read must write nothing at all");
}

/// Seeding twice changes nothing the second time — the property the
/// dispatcher's boot depends on, since it seeds on every start.
#[tokio::test(flavor = "multi_thread")]
async fn seeding_is_idempotent() {
    let db = TestDb::new().await;
    let first = seed_authored_rules(&db.pool, Path::new(RULES_DIR))
        .await
        .expect("first seed");
    let second = seed_authored_rules(&db.pool, Path::new(RULES_DIR))
        .await
        .expect("second seed");
    assert!(
        second.inserted.is_empty() && second.retired.is_empty(),
        "a second seed must write nothing (first: {first:?}, second: {second:?})"
    );
}

/// Every rule topic must be CAPTURED by the durable stream, not just
/// accepted by the consumer filter. A filter naming a subject the
/// stream doesn't ingest is legal JetStream — and delivers nothing,
/// silently: the 2026-07-10 year run shipped a rule on
/// `commerce.invoice.created` whose events were never stored, so the
/// COGS-owning consume fired zero times while every gate stayed
/// green. This test is the missing tripwire: add a rule on a new
/// topic family and the build fails until `stream_subjects()` covers
/// it.
#[test]
fn stream_covers_every_rule_topic() {
    let raw = parse_raw_path(RULES_DIR).expect("parse the rule directory");
    let subjects = boss_nats::durable::stream_subjects();
    for rule in &raw.rules {
        // Scheduled rules have no topic — nothing to cover.
        let Some(topic) = rule.on_event.as_deref() else {
            continue;
        };
        let covered = subjects.iter().any(|s| {
            let family = s.trim_end_matches('>').trim_end_matches('.');
            topic == family || topic.starts_with(&format!("{family}."))
        });
        assert!(
            covered,
            "rule {:?} listens on {topic:?}, which no stream subject captures              ({subjects:?}) — the consumer filter would accept it and deliver              NOTHING, silently; extend boss_nats::durable::stream_subjects()",
            rule.name
        );
    }
}
