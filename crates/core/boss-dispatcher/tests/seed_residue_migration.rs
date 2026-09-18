//! A rule row a migration inserted and no file ever authored again is
//! SEED RESIDUE, not history — and a fresh database no longer carries it.
//!
//! WHAT WAS MEASURED (backlog b5f21e82, 2026-09-18). Thirty-one
//! `INSERT INTO dispatcher_rules` migrations under `infra/postgres/schema/`
//! predate the collapse that made `infra/dispatcher/rules/` the
//! registry's definition (41ba00cd, 2026-09-11). They are applied
//! history and stay exactly as written, so on EVERY fresh database they
//! still insert sixty-four rule names as active product rows. Thirty-five
//! of those names no product file authors any longer: the thirty-one
//! brewery reactors moved to `examples/brewery/seeds/rules.toml` on
//! 2026-09-17 (design e2580840 car 4, backlog 105fb702) and four the
//! design-doc corpus indexer's deletion retired (f5da586c). The boot
//! seed then retired all thirty-five, on every new instance — churn in
//! every first boot log, retired history a tenant's `boss tenant
//! publish` had to TAKE OVER (70bc5725: its row lands at v(n+1), above
//! a version its file never named), and a permanent `registry ahead,
//! left alone` line per rule on every publish after that.
//!
//! The migration under test deletes those rows: a static list of names
//! derived from the tree, `source IS NULL` only, and only where no
//! tenant-sourced row holds the name (a tenant that already took a name
//! over keeps the history its takeover was judged against). Applied
//! after the inserts on a fresh database, it undoes them before the seed
//! runs, so the seed retires nothing and a tenant lands at its file's
//! version; on a converged database the rows are the retired residue,
//! and they go.
//!
//! THE PIN (CLAUDE.md §9a). The list in the migration is a copy of a
//! fact the tree holds — names inserted by migrations minus names under
//! `infra/dispatcher/rules/` — and a copy is pinned. Both directions:
//! every name listed is one a migration inserted and no product file
//! authors; every such name is listed.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use boss_dispatcher::rules::authoring::{create_draft, list_versions};
use boss_dispatcher::rules::registry::parse_raw_path;
use boss_dispatcher::rules::seed::seed_authored_rules;
use boss_testing::{TestDb, dispatcher_rules_dir, repo_root};

/// The migration this file is about. Named here rather than searched
/// for, so a test that cannot find it fails on the filename and not on
/// an empty derivation.
const RESIDUE_MIGRATION: &str = "20260918022108-seed-residue-is-not-a-retirement.sql";

/// Names a migration inserted into `dispatcher_rules` that were LATER
/// retired from the tree by a decision — a product rule file deleted
/// after this migration was written. Such a name is not residue: the
/// file existed after the collapse, its retirement was a reviewed
/// deletion, and the seed's retired row is history. Add the name here
/// with the packet that retired it when the completeness check below
/// names it; the migration itself is applied history and is never
/// edited (the checksum guard in migrate.sh refuses it).
const RETIRED_BY_DECISION_SINCE: &[&str] = &[];

fn schema_dir() -> PathBuf {
    repo_root().join("infra/postgres/schema")
}

/// Every `*.sql` under the schema directory, in apply order (numeric
/// prefix), the way migrate.sh and boss-testing's `build.rs` read it.
fn migrations() -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(schema_dir())
        .expect("read the schema directory")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("sql"))
        .collect();
    files.sort_by_key(|p| {
        p.file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.split('-').next())
            .and_then(|n| n.parse::<u64>().ok())
            .unwrap_or(u64::MAX)
    });
    files
}

/// The derivation, as text: every name a migration's `INSERT INTO
/// dispatcher_rules … VALUES` row names. Every such row in the tree
/// begins a line with `('<name>', <version>,` — the shape all
/// thirty-one historical migrations share and this reader relies on
/// (a row in any other shape would be missed here AND unpinned, which
/// the count assertion in `the_list_is_the_derivation` guards).
fn names_inserted_by(path: &Path) -> BTreeSet<String> {
    let src = std::fs::read_to_string(path).expect("read a migration");
    let mut inside = false;
    let mut names = BTreeSet::new();
    for line in src.lines() {
        let t = line.trim_start();
        if t.starts_with("--") {
            continue;
        }
        let lower = t.to_ascii_lowercase();
        if lower.starts_with("insert into dispatcher_rules")
            && lower["insert into dispatcher_rules".len()..]
                .chars()
                .next()
                .is_none_or(|c| c == ' ' || c == '(' || c == '\t')
        {
            inside = true;
        }
        if inside
            && t.starts_with("('")
            && let Some((name, rest)) = t[2..].split_once('\'')
        {
            let rest = rest.trim_start_matches(',').trim_start();
            let version_follows = rest
                .split(',')
                .next()
                .is_some_and(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()));
            if version_follows {
                names.insert(name.to_string());
            }
        }
        if inside && t.trim_end().ends_with(';') {
            inside = false;
        }
    }
    names
}

fn names_inserted_by_migrations() -> BTreeSet<String> {
    migrations()
        .iter()
        .filter(|p| p.file_name().and_then(|n| n.to_str()) != Some(RESIDUE_MIGRATION))
        .flat_map(|p| names_inserted_by(p))
        .collect()
}

fn names_the_tree_authors() -> BTreeSet<String> {
    parse_raw_path(dispatcher_rules_dir())
        .expect("parse the authored registry")
        .rules
        .into_iter()
        .map(|r| r.name)
        .collect()
}

/// The migration's own list: one quoted name per line inside its
/// `name IN (…)`. Read from the file the database applies, not from a
/// second copy in this test.
fn names_the_migration_deletes() -> BTreeSet<String> {
    let src = std::fs::read_to_string(schema_dir().join(RESIDUE_MIGRATION))
        .unwrap_or_else(|e| panic!("read {RESIDUE_MIGRATION}: {e}"));
    src.lines()
        .map(str::trim)
        .filter(|l| !l.starts_with("--"))
        .filter_map(|l| {
            let l = l.strip_suffix(',').unwrap_or(l);
            l.strip_prefix('\'')
                .and_then(|l| l.strip_suffix('\''))
                .filter(|n| !n.is_empty() && !n.contains('\''))
                .map(str::to_string)
        })
        .collect()
}

/// The list equals the derivation, both ways, and the derivation read
/// something: sixty-four names as of 2026-09-18, which cannot grow
/// (`infra/lint/no-migration-writes-a-dispatcher-rule.sh` refuses a
/// new INSERT) and cannot shrink (applied history is never edited).
#[test]
fn the_list_is_the_derivation() {
    let inserted = names_inserted_by_migrations();
    assert!(
        inserted.len() >= 60,
        "the derivation read {} names from the historical migrations; sixty-four were \
         measured on 2026-09-18, so the reader in this test no longer matches the \
         migrations' row shape",
        inserted.len()
    );
    let authored = names_the_tree_authors();
    let residue: BTreeSet<String> = inserted
        .difference(&authored)
        .filter(|n| !RETIRED_BY_DECISION_SINCE.contains(&n.as_str()))
        .cloned()
        .collect();
    let listed = names_the_migration_deletes();

    let unlisted: Vec<&String> = residue.difference(&listed).collect();
    assert!(
        unlisted.is_empty(),
        "a migration inserted these names and no product file authors them, but \
         {RESIDUE_MIGRATION} does not list them: {unlisted:?}. If the file was deleted \
         AFTER that migration was written, its retirement was a decision and the rows are \
         history — add the name to RETIRED_BY_DECISION_SINCE with the packet that retired it."
    );
    let extra: Vec<&String> = listed.difference(&residue).collect();
    assert!(
        extra.is_empty(),
        "{RESIDUE_MIGRATION} lists names that are not seed residue — either no migration \
         inserted them, or a product file authors them: {extra:?}"
    );
    assert!(
        !listed.is_empty(),
        "the migration's list read as empty: the one-name-per-line shape this test parses \
         no longer matches the file"
    );
}

/// A fresh database — the full schema, the way TestDb applies it and
/// migrate.sh applies it on a new instance — holds NO row for a moved
/// name before the seed runs, and the seed then retires nothing.
#[tokio::test(flavor = "multi_thread")]
async fn a_fresh_database_carries_no_seed_residue() {
    let db = TestDb::new().await;
    let listed: Vec<String> = names_the_migration_deletes().into_iter().collect();
    assert!(
        listed.contains(&"spawn-tasting-panel-on-brew-close".to_string()),
        "the moved brewery reactor is on the list, or this test proves nothing"
    );

    let leftover: Vec<String> =
        sqlx::query_scalar("SELECT name FROM dispatcher_rules WHERE name = ANY($1) ORDER BY name")
            .bind(&listed)
            .fetch_all(&db.pool)
            .await
            .expect("query the residue names");
    assert!(
        leftover.is_empty(),
        "a fresh database still carries migration-inserted rows no product file authors, \
         before the seed has run: {leftover:?}"
    );

    let report = seed_authored_rules(&db.pool, dispatcher_rules_dir())
        .await
        .expect("seed the authored registry");
    assert!(
        report.retired.is_empty(),
        "with the residue gone, a fresh database's first seed has nothing to retire — it \
         retired {:?}",
        report.retired
    );
    assert!(
        report.rejected.is_empty(),
        "the seed rejected {:?}",
        report.rejected
    );
}

/// A tenant's file lands at the version it declares on a fresh
/// instance — no retired history under the name, so no takeover and no
/// `registry ahead` line afterwards. The brewery's own file is used, so
/// the version compared is the one the tenant ships.
#[tokio::test(flavor = "multi_thread")]
async fn a_tenant_lands_at_its_files_version_on_a_fresh_database() {
    let db = TestDb::new().await;
    let name = "keg-deposit-settle-on-keg-return-closed";
    let brewery = parse_raw_path(repo_root().join("examples/brewery/seeds/rules.toml"))
        .expect("parse the brewery tenant's rules.toml");
    let rule = brewery
        .rules
        .iter()
        .find(|r| r.name == name)
        .expect("the brewery declares the keg-deposit settle reactor");

    assert!(
        list_versions(&db.pool, name)
            .await
            .expect("list versions")
            .is_empty(),
        "no history under the name before the tenant publishes"
    );
    let draft = create_draft(&db.pool, rule, Some("tenant:brewery"))
        .await
        .expect("the name is free on a fresh database");
    assert_eq!(
        draft.version, rule.version as i32,
        "the tenant's row takes the version its file declares — not one above a retired \
         product row the migrations used to leave"
    );
    assert_eq!(draft.source.as_deref(), Some("tenant:brewery"));
}

/// The migration's own predicate, run against fixture rows: a listed
/// name with only product-sourced rows goes; a listed name a tenant
/// already holds keeps BOTH its rows (the takeover's history stays);
/// an unlisted product row is untouched.
#[tokio::test(flavor = "multi_thread")]
async fn the_migration_deletes_only_unowned_residue() {
    let db = TestDb::new().await;
    let sql =
        std::fs::read_to_string(schema_dir().join(RESIDUE_MIGRATION)).expect("read the migration");
    let mut listed = names_the_migration_deletes().into_iter();
    let gone = listed.next().expect("a first listed name");
    let taken_over = listed.next().expect("a second listed name");
    let unlisted = "zzz-a-name-no-migration-lists";

    let insert = |name: &str, version: i32, status: &str, source: Option<&str>| {
        let name = name.to_string();
        let status = status.to_string();
        let source = source.map(str::to_string);
        let pool = db.pool.clone();
        async move {
            sqlx::query(
                "INSERT INTO dispatcher_rules (name, version, status, on_event, when_expr, do_steps, source) \
                 VALUES ($1, $2, $3, 'jobs.job.closed', NULL, \
                         '[{\"handler\":\"messages.notify\",\"args\":{}}]'::jsonb, $4)",
            )
            .bind(name)
            .bind(version)
            .bind(status)
            .bind(source)
            .execute(&pool)
            .await
            .expect("insert a fixture row");
        }
    };
    // The converged instance's shape for a name nobody took over.
    insert(&gone, 1, "retired", None).await;
    // The playground's shape: the product's retired row and the
    // tenant's takeover above it.
    insert(&taken_over, 1, "retired", None).await;
    insert(&taken_over, 2, "active", Some("tenant:brewery")).await;
    insert(unlisted, 1, "retired", None).await;

    sqlx::raw_sql(&sql)
        .execute(&db.pool)
        .await
        .expect("apply the migration again over the fixture rows");

    let rows = |name: &str| {
        let name = name.to_string();
        let pool = db.pool.clone();
        async move {
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM dispatcher_rules WHERE name = $1")
                .bind(name)
                .fetch_one(&pool)
                .await
                .expect("count rows")
        }
    };
    assert_eq!(rows(&gone).await, 0, "unowned residue goes: {gone}");
    assert_eq!(
        rows(&taken_over).await,
        2,
        "a name a tenant holds keeps the history its takeover was judged against: {taken_over}"
    );
    assert_eq!(rows(unlisted).await, 1, "an unlisted row is not touched");
}
