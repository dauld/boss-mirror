//! Enum ↔ schema agreement: every `DocStatus` value must be
//! insertable into `design_docs` (the CHECK constraint) and read back
//! through the adapter. Three copies of this vocabulary drifted apart
//! silently — the Rust enum, the DB CHECK (missing `reopened` AND
//! `living`), and the adapter's row match — and reindex failed at the
//! storage layer for any doc carrying the missing values. This test
//! pins all three together: add a variant anywhere without the others
//! and CI fails with the exact value.

use boss_docs::PgDocsRepo;
use boss_docs::port::DocsRepository;
use boss_docs::types::{DesignDoc, DocStatus};
use boss_testing::TestDb;

#[tokio::test(flavor = "multi_thread")]
async fn schema_matches_doc_status_enum() {
    let db = TestDb::new().await;
    let repo = PgDocsRepo::new(db.pool.clone());
    let now = chrono::Utc::now();
    for status in [
        DocStatus::Draft,
        DocStatus::InReview,
        DocStatus::Approved,
        DocStatus::Shipped,
        DocStatus::Reopened,
        DocStatus::Superseded,
        DocStatus::Living,
    ] {
        let doc = DesignDoc {
            path: format!("docs/design/_status-{}.md", status.as_str()),
            title: format!("status {}", status.as_str()),
            status,
            word_count: 1,
            last_modified: now,
            last_author: "test".into(),
            last_indexed_at: now,
            last_commit_sha: "test".into(),
            content_html: "<p>x</p>".into(),
        };
        repo.upsert_doc(&doc, &[]).await.unwrap_or_else(|e| {
            panic!(
                "status `{}` rejected by the schema or adapter: {e}",
                status.as_str()
            )
        });
        let read = repo
            .doc_by_path(&doc.path)
            .await
            .unwrap()
            .expect("doc reads back");
        assert_eq!(read.status, status, "round-trip changed the status");
    }
}
