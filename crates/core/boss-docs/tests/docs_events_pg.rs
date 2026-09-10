//! S1 of the design-review dogfooding arc (`e556c000`): the docs
//! subsystem joins the event loop. Before this, boss-docs emitted
//! NOTHING — zero `docs.*` events in 307k audit rows — so no
//! dispatcher rule could ever sequence a review, and every lifecycle
//! transition was human hands (David: "we aren't really dogfooding
//! our own software yet").
//!
//! Contracts pinned:
//! 1. First index of a doc emits `docs.design.indexed` through the
//!    TRANSACTIONAL OUTBOX (same tx as the row — the phase-2 recipe),
//!    carrying open/resolved question counts.
//! 2. An identical re-upsert emits NOTHING. The startup auto-reindex
//!    re-upserts every doc on every boot; without change-detection
//!    the log gains ~23 noise events per restart and any
//!    spawn-review rule fires forever.
//! 3. A change in the question set (a question resolving) emits again
//!    with the new counts.

use boss_docs::port::DocsRepository;
use boss_docs::postgres::PgDocsRepo;
use boss_docs::types::{DesignDoc, DesignQuestion};
use boss_testing::TestDb;

fn doc(path: &str) -> DesignDoc {
    DesignDoc {
        path: path.into(),
        title: "A doc".into(),
        status: boss_docs::types::DocStatus::Draft,
        word_count: 100,
        last_modified: chrono::Utc::now(),
        last_author: "tester".into(),
        last_indexed_at: chrono::Utc::now(),
        last_commit_sha: "abc123".into(),
        content_html: "<p>hi</p>".into(),
    }
}

fn q(path: &str, anchor: &str, resolved: bool) -> DesignQuestion {
    DesignQuestion {
        id: format!("{path}#{anchor}"),
        doc_path: path.into(),
        anchor: anchor.into(),
        ordinal: anchor.trim_start_matches('Q').parse().unwrap_or(0),
        title: format!("{anchor} title"),
        body_md: "body".into(),
        proposal: None,
        context_md: None,
        resolved,
    }
}

async fn outbox_kinds(pool: &sqlx::PgPool) -> Vec<(String, serde_json::Value)> {
    sqlx::query_as::<_, (String, serde_json::Value)>(
        "SELECT kind, payload FROM event_outbox WHERE source = 'docs' ORDER BY id",
    )
    .fetch_all(pool)
    .await
    .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn index_emits_on_first_and_on_change_never_on_identical() {
    let db = TestDb::new().await;
    let repo = PgDocsRepo::new(db.pool.clone());
    let path = "docs/design/example.md";

    // 1. First index: one event, correct counts.
    repo.upsert_doc(&doc(path), &[q(path, "Q1", false), q(path, "Q2", false)])
        .await
        .unwrap();
    let events = outbox_kinds(&db.pool).await;
    assert_eq!(events.len(), 1, "first index emits once: {events:?}");
    assert_eq!(events[0].0, "docs.design.indexed");
    assert_eq!(events[0].1["path"], path);
    assert_eq!(events[0].1["open_questions"], 2);
    assert_eq!(events[0].1["resolved_questions"], 0);

    // 2. Identical re-upsert (the startup reindex): silence.
    repo.upsert_doc(&doc(path), &[q(path, "Q1", false), q(path, "Q2", false)])
        .await
        .unwrap();
    assert_eq!(
        outbox_kinds(&db.pool).await.len(),
        1,
        "an unchanged re-index must not emit — every boot re-upserts every doc"
    );

    // 3. A question resolves: emit with new counts.
    repo.upsert_doc(&doc(path), &[q(path, "Q1", true), q(path, "Q2", false)])
        .await
        .unwrap();
    let events = outbox_kinds(&db.pool).await;
    assert_eq!(events.len(), 2, "the change emits: {events:?}");
    assert_eq!(events[1].1["open_questions"], 1);
    assert_eq!(events[1].1["resolved_questions"], 1);
}
