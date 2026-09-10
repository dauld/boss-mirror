//! Postgres adapter for `DocsRepository`.

use std::collections::HashSet;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::{PgPool, Row, postgres::PgRow};
use uuid::Uuid;

use crate::port::{DocsError, DocsRepository};
use crate::types::{
    DesignDoc, DesignQuestion, DocStatus, RejectedDocRecord, apply_recorded_decisions,
};

pub struct PgDocsRepo {
    pool: PgPool,
}

impl PgDocsRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

fn doc_from_row(row: &PgRow) -> Result<DesignDoc, DocsError> {
    let status_str: String = row
        .try_get("status")
        .map_err(|e| DocsError::Storage(e.to_string()))?;
    let status = match DocStatus::from_db_str(&status_str) {
        Some(s) => s,
        None => return Err(DocsError::Storage(format!("unknown status {status_str}"))),
    };
    Ok(DesignDoc {
        path: row.try_get("path").map_err(storage)?,
        title: row.try_get("title").map_err(storage)?,
        status,
        word_count: row.try_get("word_count").map_err(storage)?,
        last_modified: row.try_get("last_modified").map_err(storage)?,
        last_author: row.try_get("last_author").map_err(storage)?,
        last_indexed_at: row.try_get("last_indexed_at").map_err(storage)?,
        last_commit_sha: row.try_get("last_commit_sha").map_err(storage)?,
        content_html: row.try_get("content_html").map_err(storage)?,
    })
}

fn question_from_row(row: &PgRow) -> Result<DesignQuestion, DocsError> {
    Ok(DesignQuestion {
        id: row.try_get("id").map_err(storage)?,
        doc_path: row.try_get("doc_path").map_err(storage)?,
        anchor: row.try_get("anchor").map_err(storage)?,
        ordinal: row.try_get("ordinal").map_err(storage)?,
        title: row.try_get("title").map_err(storage)?,
        body_md: row.try_get("body_md").map_err(storage)?,
        proposal: row.try_get("proposal").map_err(storage)?,
        context_md: row.try_get("context_md").map_err(storage)?,
        resolved: row.try_get("resolved").map_err(storage)?,
    })
}

fn storage<E: std::fmt::Display>(e: E) -> DocsError {
    DocsError::Storage(e.to_string())
}

#[async_trait]
impl DocsRepository for PgDocsRepo {
    async fn all_docs(&self) -> Result<Vec<DesignDoc>, DocsError> {
        let rows = sqlx::query("SELECT * FROM design_docs ORDER BY path")
            .fetch_all(&self.pool)
            .await
            .map_err(storage)?;
        rows.iter().map(doc_from_row).collect()
    }

    async fn doc_by_path(&self, path: &str) -> Result<Option<DesignDoc>, DocsError> {
        let row = sqlx::query("SELECT * FROM design_docs WHERE path = $1")
            .bind(path)
            .fetch_optional(&self.pool)
            .await
            .map_err(storage)?;
        row.as_ref().map(doc_from_row).transpose()
    }

    async fn questions_for_doc(&self, path: &str) -> Result<Vec<DesignQuestion>, DocsError> {
        let rows =
            sqlx::query("SELECT * FROM design_questions WHERE doc_path = $1 ORDER BY ordinal")
                .bind(path)
                .fetch_all(&self.pool)
                .await
                .map_err(storage)?;
        rows.iter().map(question_from_row).collect()
    }

    async fn upsert_doc(
        &self,
        doc: &DesignDoc,
        questions: &[DesignQuestion],
    ) -> Result<(), DocsError> {
        let mut tx = self.pool.begin().await.map_err(storage)?;

        // The packet decides, not the file. `questions` arrives with
        // `resolved` parsed from the markdown heading; a decision
        // recorded against an anchor resolves it regardless. See
        // `apply_recorded_decisions` for why — briefly, the reinsert
        // below wipes the question set on every index, so an answer
        // that lives only on the review packet would be forgotten and
        // the doc would re-spawn a review for a question already
        // answered.
        //
        // `design_recorded_decisions` is the closed ledger of every
        // answer ever recorded against an anchor — the rows the
        // pending-decision table held, plus the answers stranded in
        // flush-job payloads, backfilled when that pipeline was deleted
        // (migration 202609101200). It is READ-ONLY now: nothing writes
        // a row, and nothing writes `(resolved)` into a heading either,
        // so this merge is the only thing that keeps an answered
        // question from re-opening. Measured the day the pipeline was
        // deleted: 25 questions across 6 docs.
        let decided: HashSet<String> = sqlx::query_scalar::<_, String>(
            "SELECT anchor FROM design_recorded_decisions WHERE doc_path = $1",
        )
        .bind(&doc.path)
        .fetch_all(&mut *tx)
        .await
        .map_err(storage)?
        .into_iter()
        .collect();
        let questions = apply_recorded_decisions(questions, &decided);
        let questions = &questions[..];

        // Change detection for the `docs.design.indexed` event
        // (dogfooding arc e556c000, S1): the startup auto-reindex
        // re-upserts every doc on every boot, so emitting
        // unconditionally would spam the log ~23 events per restart
        // and re-fire any spawn-review rule forever. Emit only when
        // the review-relevant surface moved: title, status, or the
        // open/resolved question counts.
        let prior: Option<(String, String, i64, i64)> = sqlx::query_as(
            "SELECT d.title, d.status,
                    count(*) FILTER (WHERE NOT q.resolved),
                    count(*) FILTER (WHERE q.resolved)
             FROM design_docs d
             LEFT JOIN design_questions q ON q.doc_path = d.path
             WHERE d.path = $1
             GROUP BY d.title, d.status",
        )
        .bind(&doc.path)
        .fetch_optional(&mut *tx)
        .await
        .map_err(storage)?;
        let open = questions.iter().filter(|q| !q.resolved).count() as i64;
        let resolved = questions.iter().filter(|q| q.resolved).count() as i64;
        let changed = match &prior {
            None => true,
            Some((t, s, o, r)) => {
                t != &doc.title || s != doc.status.as_str() || *o != open || *r != resolved
            }
        };

        sqlx::query(
            "INSERT INTO design_docs (
                path, title, status, word_count,
                last_modified, last_author, last_indexed_at,
                last_commit_sha, content_html
             ) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9)
             ON CONFLICT (path) DO UPDATE SET
                title = EXCLUDED.title,
                status = EXCLUDED.status,
                word_count = EXCLUDED.word_count,
                last_modified = EXCLUDED.last_modified,
                last_author = EXCLUDED.last_author,
                last_indexed_at = EXCLUDED.last_indexed_at,
                last_commit_sha = EXCLUDED.last_commit_sha,
                content_html = EXCLUDED.content_html",
        )
        .bind(&doc.path)
        .bind(&doc.title)
        .bind(doc.status.as_str())
        .bind(doc.word_count)
        .bind(doc.last_modified)
        .bind(&doc.last_author)
        .bind(doc.last_indexed_at)
        .bind(&doc.last_commit_sha)
        .bind(&doc.content_html)
        .execute(&mut *tx)
        .await
        .map_err(storage)?;

        if changed {
            let event = boss_core::event::Event {
                id: Uuid::new_v4(),
                timestamp: Utc::now(),
                source: "docs".to_string(),
                kind: "docs.design.indexed".to_string(),
                payload: serde_json::json!({
                    "path": doc.path,
                    "title": doc.title,
                    "status": doc.status.as_str(),
                    "open_questions": open,
                    "resolved_questions": resolved,
                    "first_index": prior.is_none(),
                }),
            };
            boss_events::outbox::record_event_in_tx(&mut tx, &event)
                .await
                .map_err(storage)?;
        }

        // Delete-and-reinsert the question set for this doc.
        sqlx::query("DELETE FROM design_questions WHERE doc_path = $1")
            .bind(&doc.path)
            .execute(&mut *tx)
            .await
            .map_err(storage)?;

        for q in questions {
            sqlx::query(
                "INSERT INTO design_questions (
                    id, doc_path, anchor, ordinal, title, body_md,
                    proposal, context_md, resolved
                 ) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9)",
            )
            .bind(&q.id)
            .bind(&q.doc_path)
            .bind(&q.anchor)
            .bind(q.ordinal)
            .bind(&q.title)
            .bind(&q.body_md)
            .bind(&q.proposal)
            .bind(&q.context_md)
            .bind(q.resolved)
            .execute(&mut *tx)
            .await
            .map_err(storage)?;
        }

        tx.commit().await.map_err(storage)?;
        Ok(())
    }

    async fn all_rejections(&self) -> Result<Vec<RejectedDocRecord>, DocsError> {
        let rows: Vec<(String, String, DateTime<Utc>, DateTime<Utc>)> = sqlx::query_as(
            "SELECT path, reason, first_seen_at, last_seen_at \
             FROM design_doc_rejections ORDER BY first_seen_at",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(storage)?;
        Ok(rows
            .into_iter()
            .map(
                |(path, reason, first_seen_at, last_seen_at)| RejectedDocRecord {
                    path,
                    reason,
                    first_seen_at,
                    last_seen_at,
                },
            )
            .collect())
    }

    async fn upsert_rejection(&self, path: &str, reason: &str) -> Result<(), DocsError> {
        // first_seen_at deliberately NOT updated on conflict: a doc
        // failing for six days should say six days, not "just now".
        sqlx::query(
            "INSERT INTO design_doc_rejections (path, reason) VALUES ($1, $2) \
             ON CONFLICT (path) DO UPDATE SET reason = EXCLUDED.reason, last_seen_at = NOW()",
        )
        .bind(path)
        .bind(reason)
        .execute(&self.pool)
        .await
        .map_err(storage)?;
        Ok(())
    }

    async fn clear_rejection(&self, path: &str) -> Result<(), DocsError> {
        sqlx::query("DELETE FROM design_doc_rejections WHERE path = $1")
            .bind(path)
            .execute(&self.pool)
            .await
            .map_err(storage)?;
        Ok(())
    }

    async fn delete_doc(&self, path: &str) -> Result<(), DocsError> {
        // Cascade handles design_questions. The recorded-decision
        // ledger has no FK to design_docs (it is keyed by
        // (doc_path, anchor) as free text), so it is cleared
        // explicitly.
        //
        // WHY THE EXPLICIT DELETE MATTERS. `design_flush_jobs` used to
        // carry a non-cascading FK to `design_docs`, so a doc with
        // flush-job history could not be deleted at all — and the
        // reindex prune is the only caller. One doc removed from disk
        // therefore failed the WHOLE reindex:
        //
        //   update or delete on table "design_docs" violates foreign key
        //   constraint "design_flush_jobs_doc_path_fkey"
        //
        // Which is what happened. `event-kind-registry.md` was folded
        // away per the docs lifecycle, kept its flush jobs, and every
        // reindex since returned 500 — so the tracker stopped learning
        // about the corpus while continuing to look healthy from the
        // outside. That table is gone; the lesson is why this delete is
        // explicit and unconditional rather than left to a constraint.
        //
        // The ledger rows go with the doc: they answer questions in a
        // file that no longer exists. Their history is preserved in the
        // audit log, which is the durable record.
        let mut tx = self.pool.begin().await.map_err(storage)?;
        sqlx::query("DELETE FROM design_recorded_decisions WHERE doc_path = $1")
            .bind(path)
            .execute(&mut *tx)
            .await
            .map_err(storage)?;
        sqlx::query("DELETE FROM design_docs WHERE path = $1")
            .bind(path)
            .execute(&mut *tx)
            .await
            .map_err(storage)?;
        tx.commit().await.map_err(storage)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::DocStatus;
    use boss_testing::TestDb;
    use chrono::TimeZone;

    fn sample_doc(path: &str) -> DesignDoc {
        DesignDoc {
            path: path.to_string(),
            title: "Test Doc".to_string(),
            status: DocStatus::InReview,
            word_count: 100,
            last_modified: Utc.with_ymd_and_hms(2026, 4, 13, 12, 0, 0).unwrap(),
            last_author: "alice".to_string(),
            last_indexed_at: Utc::now(),
            last_commit_sha: "abc123".to_string(),
            content_html: "<h1>Test</h1>".to_string(),
        }
    }

    fn sample_question(doc_path: &str, anchor: &str, ordinal: i32) -> DesignQuestion {
        DesignQuestion {
            id: format!("{doc_path}#{anchor}"),
            doc_path: doc_path.to_string(),
            anchor: anchor.to_string(),
            ordinal,
            title: format!("Question {anchor}"),
            body_md: "body".to_string(),
            proposal: Some("proposal".to_string()),
            context_md: None,
            resolved: false,
        }
    }

    /// Stand a ledger row up. Nothing in production writes this table
    /// any more — the flush pipeline that did was deleted on
    /// 2026-09-10 and the rows were backfilled by its migration — so
    /// there is no port method to call and the test writes the row it
    /// is asserting about.
    async fn record(db: &TestDb, doc_path: &str, anchor: &str) {
        sqlx::query(
            "INSERT INTO design_recorded_decisions
                 (id, doc_path, anchor, kind, resolution, decided_by)
             VALUES ($1,$2,$3,'override',$4,'david@algedonic.dev')",
        )
        .bind(format!("{doc_path}#{anchor}"))
        .bind(doc_path)
        .bind(anchor)
        .bind("WireGuard")
        .execute(&db.pool)
        .await
        .expect("seed a recorded decision");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn upsert_and_list_docs() {
        let db = TestDb::new().await;
        let repo = PgDocsRepo::new(db.pool.clone());
        repo.upsert_doc(&sample_doc("docs/design/a.md"), &[])
            .await
            .unwrap();
        repo.upsert_doc(&sample_doc("docs/design/b.md"), &[])
            .await
            .unwrap();
        let docs = repo.all_docs().await.unwrap();
        assert_eq!(docs.len(), 2);
    }

    /// The ledger read in `upsert_doc` is the whole fix, and only a
    /// real database exercises it. Two answers recorded, one question
    /// never answered; no doc heading says `(resolved)`, and since the
    /// flush pipeline is gone none ever will — so this merge is the
    /// only thing keeping an answered question from re-opening and
    /// spawning a duplicate review (David, 2026-08-18: "I keep seeing
    /// jobs that I have responded to").
    #[tokio::test(flavor = "multi_thread")]
    async fn recorded_answers_resolve_the_questions_the_file_still_calls_open() {
        let db = TestDb::new().await;
        let repo = PgDocsRepo::new(db.pool.clone());
        let doc = sample_doc("docs/design/a.md");
        let parsed = || {
            vec![
                sample_question("docs/design/a.md", "Q1", 0),
                sample_question("docs/design/a.md", "Q2", 1),
                sample_question("docs/design/a.md", "Q3", 2),
            ]
        };
        repo.upsert_doc(&doc, &parsed()).await.unwrap();

        for anchor in ["Q1", "Q2"] {
            record(&db, "docs/design/a.md", anchor).await;
        }

        repo.upsert_doc(&doc, &parsed()).await.unwrap();

        let qs = repo.questions_for_doc("docs/design/a.md").await.unwrap();
        let by_anchor = |a: &str| qs.iter().find(|q| q.anchor == a).unwrap().resolved;
        assert!(by_anchor("Q1"), "Q1 carries a recorded answer");
        assert!(by_anchor("Q2"), "Q2 carries a recorded answer");
        assert!(!by_anchor("Q3"), "Q3 was never answered — still open");
    }

    /// Pruning a doc clears its ledger rows. They answer questions in
    /// a file that no longer exists, and the reindex prune is the only
    /// caller — one un-deletable row used to fail the WHOLE reindex.
    #[tokio::test(flavor = "multi_thread")]
    async fn deleting_a_doc_clears_its_recorded_decisions() {
        let db = TestDb::new().await;
        let repo = PgDocsRepo::new(db.pool.clone());
        repo.upsert_doc(
            &sample_doc("docs/design/a.md"),
            &[sample_question("docs/design/a.md", "Q1", 0)],
        )
        .await
        .unwrap();
        record(&db, "docs/design/a.md", "Q1").await;
        repo.delete_doc("docs/design/a.md").await.unwrap();
        let left: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM design_recorded_decisions WHERE doc_path = $1",
        )
        .bind("docs/design/a.md")
        .fetch_one(&db.pool)
        .await
        .unwrap();
        assert_eq!(left, 0, "the ledger rows go with the doc");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn upsert_doc_replaces_questions() {
        let db = TestDb::new().await;
        let repo = PgDocsRepo::new(db.pool.clone());
        let doc = sample_doc("docs/design/a.md");
        repo.upsert_doc(
            &doc,
            &[
                sample_question("docs/design/a.md", "Q1", 0),
                sample_question("docs/design/a.md", "Q2", 1),
            ],
        )
        .await
        .unwrap();
        assert_eq!(
            repo.questions_for_doc("docs/design/a.md")
                .await
                .unwrap()
                .len(),
            2
        );
        repo.upsert_doc(&doc, &[sample_question("docs/design/a.md", "Q1", 0)])
            .await
            .unwrap();
        assert_eq!(
            repo.questions_for_doc("docs/design/a.md")
                .await
                .unwrap()
                .len(),
            1
        );
    }
}
