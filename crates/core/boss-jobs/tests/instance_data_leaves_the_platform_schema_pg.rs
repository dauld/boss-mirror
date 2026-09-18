//! A fresh database carries no instance data (backlog ee368d0c; design
//! 42277636 first wave, audit H5).
//!
//! Measured 2026-09-18: nine migrations seeded this LAN's nodes, their
//! roles, eight service instances and one operator's credential ids
//! into every database BOSS ever created, and the boot eviction covered
//! none of those tables. 20260918063829-instance-data-leaves-the-
//! platform-schema.sql deletes what they seeded where nothing
//! references it. This file holds two things to that migration:
//!
//! - on the bare schema, before any declaration, NO node with an
//!   address on this LAN, no node role, no service instance, no
//!   credential and no estate identity row exists — while the
//!   vocabulary (the `node` Classes, the subject kinds) stays;
//! - the id lists the migration names are exactly the ids the earlier
//!   migrations INSERTed, re-derived from this directory's text in
//!   both directions (CLAUDE.md §9a) — a migration added later that
//!   seeds an instance row again is named here.

use std::collections::BTreeSet;
use std::path::Path;

use boss_testing::TestDb;

const MIGRATION: &str = "20260918063829-instance-data-leaves-the-platform-schema.sql";

#[tokio::test(flavor = "multi_thread")]
async fn the_bare_schema_holds_no_node_role_instance_or_credential() {
    let db = TestDb::new().await;
    let count = |sql: &'static str| {
        let pool = db.pool.clone();
        async move {
            sqlx::query_scalar::<_, i64>(sql)
                .fetch_one(&pool)
                .await
                .unwrap_or_else(|e| panic!("{sql}: {e}"))
        }
    };
    assert_eq!(count("SELECT count(*) FROM nodes").await, 0, "no node");
    assert_eq!(
        count("SELECT count(*) FROM nodes WHERE address LIKE '10.20.0.%'").await,
        0,
        "no node on this LAN in particular"
    );
    assert_eq!(
        count("SELECT count(*) FROM node_roles").await,
        0,
        "no node role"
    );
    assert_eq!(
        count("SELECT count(*) FROM service_instances").await,
        0,
        "no service instance"
    );
    assert_eq!(
        count("SELECT count(*) FROM credentials").await,
        0,
        "no credential"
    );
    assert_eq!(
        count("SELECT count(*) FROM subjects WHERE kind IN ('node', 'service-instance')").await,
        0,
        "no estate identity row"
    );
    // The vocabulary stays: a role is a Class of `node`, and the two
    // subject kinds are the platform's — an adopter declares nodes
    // against them.
    assert!(
        count("SELECT count(*) FROM classes WHERE subject_kind = 'node'").await >= 4,
        "the node role Classes are vocabulary, not instance data"
    );
    assert_eq!(
        count("SELECT count(*) FROM subject_kinds WHERE kind IN ('node', 'service-instance')")
            .await,
        2
    );
    // The two declaration facts are declared, so neither rides inside
    // a passing audit-integrity run unread.
    assert_eq!(
        count("SELECT count(*) FROM event_kinds WHERE kind_pattern IN ('credential.declared', 'node.declared')").await,
        2
    );
}

/// Every `INSERT INTO <table> … VALUES` statement in a migration that
/// precedes the deletion, its first quoted literal per row (the id).
fn ids_inserted_before(schema: &Path, table: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut files: Vec<_> = std::fs::read_dir(schema)
        .expect("schema dir")
        .map(|e| e.expect("entry").path())
        .filter(|p| p.extension().is_some_and(|x| x == "sql"))
        .collect();
    files.sort();
    let header = format!("INSERT INTO {table}");
    for f in files {
        let name = f.file_name().unwrap().to_string_lossy().to_string();
        if name.as_str() >= MIGRATION {
            continue;
        }
        let text = std::fs::read_to_string(&f).unwrap();
        let mut rest = text.as_str();
        while let Some(at) = rest.find(&header) {
            let stmt = &rest[at..];
            // Every seed ends `ON CONFLICT … DO NOTHING;` — a prose
            // `;` inside a notes literal must not end the statement.
            let end = stmt
                .find("ON CONFLICT")
                .or_else(|| stmt.find(';'))
                .unwrap_or(stmt.len());
            let stmt = &stmt[..end];
            rest = &rest[at + header.len()..];
            // `INSERT INTO nodes` must not read `INSERT INTO node_roles`.
            if !stmt[header.len()..]
                .chars()
                .next()
                .is_some_and(|c| c.is_whitespace() || c == '(')
            {
                continue;
            }
            let Some(v) = stmt.find("VALUES") else {
                continue;
            };
            let body = &stmt[v + "VALUES".len()..];
            // A row starts at `(` followed by whitespace and a quoted
            // literal — the id column is always first in these seeds.
            let mut i = 0;
            let b = body.as_bytes();
            while i < b.len() {
                if b[i] == b'(' {
                    let mut j = i + 1;
                    while j < b.len() && (b[j] as char).is_whitespace() {
                        j += 1;
                    }
                    if j < b.len() && b[j] == b'\'' {
                        let close = body[j + 1..].find('\'').map(|c| j + 1 + c);
                        if let Some(c) = close {
                            out.insert(body[j + 1..c].to_string());
                            i = c;
                        }
                    }
                }
                i += 1;
            }
        }
    }
    out
}

/// The quoted names inside the `IN (` list of the migration's DELETE
/// on `table`, one per line.
fn ids_the_migration_deletes(schema: &Path, table: &str) -> BTreeSet<String> {
    let text = std::fs::read_to_string(schema.join(MIGRATION)).unwrap();
    let header = format!("DELETE FROM {table} ");
    let at = text
        .find(&header)
        .unwrap_or_else(|| panic!("{MIGRATION} deletes from {table}"));
    let stmt = &text[at..];
    let stmt = &stmt[..stmt.find(';').unwrap()];
    let list = &stmt[stmt.find("IN (").expect("an IN list") + 4..];
    list.lines()
        .map(str::trim)
        .filter(|l| l.starts_with('\''))
        .map(|l| l.trim_matches(|c| c == '\'' || c == ',').to_string())
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn the_lists_are_the_ids_the_earlier_migrations_inserted() {
    let schema = boss_testing::repo_root().join("infra/postgres/schema");
    for table in ["credentials", "service_instances", "nodes"] {
        let inserted = ids_inserted_before(&schema, table);
        let deleted = ids_the_migration_deletes(&schema, table);
        assert!(!inserted.is_empty(), "{table}: the derivation read nothing");
        assert_eq!(
            deleted, inserted,
            "{table}: {MIGRATION}'s list drifted from what the earlier migrations insert"
        );
    }
    // node_roles rows are keyed by node id; the migration deletes by
    // node, so its list is the nodes list and every node a role was
    // ever seeded on is in it.
    let role_nodes = ids_inserted_before(&schema, "node_roles");
    let nodes = ids_the_migration_deletes(&schema, "node_roles");
    assert!(!role_nodes.is_empty());
    assert!(
        role_nodes.is_subset(&nodes),
        "a node_roles seed names a node outside the list: {:?}",
        role_nodes.difference(&nodes).collect::<Vec<_>>()
    );
    assert_eq!(nodes, ids_the_migration_deletes(&schema, "nodes"));
}
