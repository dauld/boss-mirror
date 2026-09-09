//! Postgres adapter for `DocsRepository`.

use std::collections::HashSet;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::Value as JsonValue;
use sqlx::{PgPool, Row, postgres::PgRow};
use uuid::Uuid;

use crate::port::{DocsError, DocsRepository};
use crate::types::{
    DecisionKind, DesignDoc, DesignQuestion, DocStatus, FlushJob, FlushJobPayload, JobStatus,
    JobStatusUpdate, PendingDecision, PendingDecisionInput, RejectedDocRecord,
    apply_recorded_decisions,
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
        pending_count: row.try_get("pending_count").map_err(storage)?,
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

fn pending_from_row(row: &PgRow) -> Result<PendingDecision, DocsError> {
    let kind_str: String = row.try_get("kind").map_err(storage)?;
    let kind = match kind_str.as_str() {
        "accept" => DecisionKind::Accept,
        "override" => DecisionKind::Override,
        other => return Err(DocsError::Storage(format!("unknown kind {other}"))),
    };
    Ok(PendingDecision {
        id: row.try_get("id").map_err(storage)?,
        doc_path: row.try_get("doc_path").map_err(storage)?,
        anchor: row.try_get("anchor").map_err(storage)?,
        kind,
        resolution: row.try_get("resolution").map_err(storage)?,
        rationale: row.try_get("rationale").map_err(storage)?,
        decided_by: row.try_get("decided_by").map_err(storage)?,
        decided_at: row.try_get("decided_at").map_err(storage)?,
    })
}

fn job_from_row(row: &PgRow) -> Result<FlushJob, DocsError> {
    let status_str: String = row.try_get("status").map_err(storage)?;
    let status = match status_str.as_str() {
        "queued" => JobStatus::Queued,
        "running" => JobStatus::Running,
        "succeeded" => JobStatus::Succeeded,
        "failed" => JobStatus::Failed,
        other => return Err(DocsError::Storage(format!("unknown status {other}"))),
    };
    let payload_json: JsonValue = row.try_get("payload").map_err(storage)?;
    let payload: FlushJobPayload = serde_json::from_value(payload_json)
        .map_err(|e| DocsError::Storage(format!("bad payload json: {e}")))?;
    Ok(FlushJob {
        id: row.try_get("id").map_err(storage)?,
        doc_path: row.try_get("doc_path").map_err(storage)?,
        status,
        requested_by: row.try_get("requested_by").map_err(storage)?,
        worked_by: row.try_get("worked_by").map_err(storage)?,
        queued_at: row.try_get("queued_at").map_err(storage)?,
        started_at: row.try_get("started_at").map_err(storage)?,
        completed_at: row.try_get("completed_at").map_err(storage)?,
        payload,
        commit_sha: row.try_get("commit_sha").map_err(storage)?,
        error: row.try_get("error").map_err(storage)?,
    })
}

fn storage<E: std::fmt::Display>(e: E) -> DocsError {
    DocsError::Storage(e.to_string())
}

/// Is this the unique-index violation that means "someone else got
/// there first"? Matched on SQLSTATE rather than on the message, which
/// is the database's to reword.
fn is_unique_violation(e: &sqlx::Error) -> bool {
    matches!(e, sqlx::Error::Database(db) if db.code().as_deref() == Some("23505"))
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
        // Both sources count, because a recorded answer moves between
        // them: `create_flush_job` snapshots the pending rows into the
        // job payload and deletes them, so once a flush is queued the
        // answer lives ONLY in that payload. Queuing a flush is not
        // writing the file — the 2026-08-18 measurement found five
        // queued jobs holding sixteen answers and twenty-nine more
        // marked failed, every one of them invisible to this count.
        // A `succeeded` job is excluded: that one did write the file,
        // so the heading now carries `(resolved)` on its own.
        // `->> 'anchor'` is nullable in the type system (a malformed
        // payload entry yields NULL), hence `Option` + `flatten`.
        let decided: HashSet<String> = sqlx::query_scalar::<_, Option<String>>(
            "SELECT anchor FROM design_pending_decisions WHERE doc_path = $1
             UNION
             SELECT d ->> 'anchor'
               FROM design_flush_jobs j,
                    LATERAL jsonb_array_elements(j.payload -> 'decisions') AS d
              WHERE j.doc_path = $1 AND j.status <> 'succeeded'",
        )
        .bind(&doc.path)
        .fetch_all(&mut *tx)
        .await
        .map_err(storage)?
        .into_iter()
        .flatten()
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
                path, title, status, pending_count, word_count,
                last_modified, last_author, last_indexed_at,
                last_commit_sha, content_html
             ) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)
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
        .bind(doc.pending_count)
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
        // Cascade handles design_questions. Pending decisions have no
        // FK to design_docs (they're keyed by (doc_path, anchor) as
        // free text), so delete them explicitly.
        //
        // Flush jobs DO have an FK (`design_flush_jobs_doc_path_fkey`)
        // and it is not a cascade, so a doc with flush-job history could
        // not be deleted at all — and the reindex prune is the only
        // caller. One doc removed from disk with a flush job attached
        // therefore failed the WHOLE reindex, not just its own prune:
        //
        //   update or delete on table "design_docs" violates foreign key
        //   constraint "design_flush_jobs_doc_path_fkey"
        //
        // Which is what happened. `event-kind-registry.md` was folded
        // away per the docs lifecycle, kept its flush jobs, and every
        // reindex since has returned 500 — so the tracker stopped
        // learning about the corpus while continuing to look healthy
        // from the outside. The upserts run before the prune and commit
        // in their own transactions, so docs still got re-parsed; only
        // the caller's answer was an error.
        //
        // The jobs go with the doc for the same reason the pending
        // decisions do: they are a queue of work to write INTO a file
        // that no longer exists. Their history is preserved in the
        // audit log, which is the durable record; this table is a
        // worklist.
        let mut tx = self.pool.begin().await.map_err(storage)?;
        sqlx::query("DELETE FROM design_flush_jobs WHERE doc_path = $1")
            .bind(path)
            .execute(&mut *tx)
            .await
            .map_err(storage)?;
        sqlx::query("DELETE FROM design_pending_decisions WHERE doc_path = $1")
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

    async fn upsert_pending_decision(
        &self,
        input: &PendingDecisionInput,
        decided_by: &str,
    ) -> Result<PendingDecision, DocsError> {
        let id = format!("pd-{}", Uuid::new_v4().simple());
        let now: DateTime<Utc> = Utc::now();
        let mut tx = self.pool.begin().await.map_err(storage)?;

        sqlx::query(
            "INSERT INTO design_pending_decisions (
                id, doc_path, anchor, kind, resolution, rationale,
                decided_by, decided_at
             ) VALUES ($1,$2,$3,$4,$5,$6,$7,$8)
             ON CONFLICT (doc_path, anchor) DO UPDATE SET
                kind = EXCLUDED.kind,
                resolution = EXCLUDED.resolution,
                rationale = EXCLUDED.rationale,
                decided_by = EXCLUDED.decided_by,
                decided_at = EXCLUDED.decided_at",
        )
        .bind(&id)
        .bind(&input.doc_path)
        .bind(&input.anchor)
        .bind(input.kind.as_str())
        .bind(&input.resolution)
        .bind(&input.rationale)
        .bind(decided_by)
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(storage)?;

        // The decision is the event the review lifecycle turns on
        // (dogfooding arc e556c000, S1): a dispatcher rule can now
        // complete the review step / queue the flush instead of a
        // human noticing.
        let event = boss_core::event::Event {
            id: Uuid::new_v4(),
            timestamp: now,
            source: "docs".to_string(),
            kind: "docs.design.decision_recorded".to_string(),
            payload: serde_json::json!({
                "doc_path": input.doc_path,
                "anchor": input.anchor,
                "kind": input.kind.as_str(),
                "resolution": input.resolution,
                "decided_by": decided_by,
            }),
        };
        boss_events::outbox::record_event_in_tx(&mut tx, &event)
            .await
            .map_err(storage)?;

        // Refresh pending_count on the doc row.
        sqlx::query(
            "UPDATE design_docs SET pending_count = (
                SELECT COUNT(*) FROM design_pending_decisions WHERE doc_path = $1
             ) WHERE path = $1",
        )
        .bind(&input.doc_path)
        .execute(&mut *tx)
        .await
        .map_err(storage)?;

        // Re-fetch the row so we return the canonical state (ON
        // CONFLICT may have kept an older id).
        let row = sqlx::query(
            "SELECT * FROM design_pending_decisions WHERE doc_path = $1 AND anchor = $2",
        )
        .bind(&input.doc_path)
        .bind(&input.anchor)
        .fetch_one(&mut *tx)
        .await
        .map_err(storage)?;
        let pending = pending_from_row(&row)?;

        tx.commit().await.map_err(storage)?;
        Ok(pending)
    }

    async fn delete_pending_decision(&self, doc_path: &str, anchor: &str) -> Result<(), DocsError> {
        let mut tx = self.pool.begin().await.map_err(storage)?;
        let result =
            sqlx::query("DELETE FROM design_pending_decisions WHERE doc_path = $1 AND anchor = $2")
                .bind(doc_path)
                .bind(anchor)
                .execute(&mut *tx)
                .await
                .map_err(storage)?;
        if result.rows_affected() == 0 {
            return Err(DocsError::NotFound(format!("{doc_path}#{anchor}")));
        }
        sqlx::query(
            "UPDATE design_docs SET pending_count = (
                SELECT COUNT(*) FROM design_pending_decisions WHERE doc_path = $1
             ) WHERE path = $1",
        )
        .bind(doc_path)
        .execute(&mut *tx)
        .await
        .map_err(storage)?;
        tx.commit().await.map_err(storage)?;
        Ok(())
    }

    async fn pending_decisions_for_doc(
        &self,
        doc_path: &str,
    ) -> Result<Vec<PendingDecision>, DocsError> {
        let rows = sqlx::query(
            "SELECT * FROM design_pending_decisions
             WHERE doc_path = $1 ORDER BY decided_at",
        )
        .bind(doc_path)
        .fetch_all(&self.pool)
        .await
        .map_err(storage)?;
        rows.iter().map(pending_from_row).collect()
    }

    async fn create_flush_job(
        &self,
        payload: &FlushJobPayload,
        requested_by: &str,
    ) -> Result<FlushJob, DocsError> {
        if payload.decisions.is_empty() {
            return Err(DocsError::BadRequest(format!(
                "no pending decisions for {}",
                payload.doc_path
            )));
        }

        let mut tx = self.pool.begin().await.map_err(storage)?;

        // Defensive: verify the pending rows still exist in the DB.
        // If the payload has N decisions but there are zero rows,
        // the caller lied and we should fail with BadRequest so the
        // UI can refetch.
        let pending_rows: Vec<PgRow> =
            sqlx::query("SELECT * FROM design_pending_decisions WHERE doc_path = $1")
                .bind(&payload.doc_path)
                .fetch_all(&mut *tx)
                .await
                .map_err(storage)?;
        if pending_rows.is_empty() {
            return Err(DocsError::BadRequest(format!(
                "no pending decisions for {}",
                payload.doc_path
            )));
        }

        let id = format!("fj-{}", Uuid::new_v4().simple());
        let now = Utc::now();
        let payload_json = serde_json::to_value(payload)
            .map_err(|e| DocsError::Storage(format!("serializing payload: {e}")))?;

        // `design_flush_jobs_one_queued_per_doc` (migration 140) is what
        // makes an answer burst settle clean. Eight decisions recorded
        // at once fire the queue rule eight times; each reads the same
        // pending rows OUTSIDE this transaction and arrives here with an
        // identical payload. Without the index all eight insert, and
        // the doc ends up with a stack of jobs claiming the same work —
        // which is what buried David's review on 2026-08-15.
        //
        // The loser reads as "already queued", which is both true and a
        // no-op: the decisions it carries are in the job that won, and
        // the caller (`docs.flush_queue`) already treats a 400 as
        // nothing to do.
        let inserted = sqlx::query(
            "INSERT INTO design_flush_jobs (
                id, doc_path, status, requested_by, queued_at, payload
             ) VALUES ($1,$2,'queued',$3,$4,$5)",
        )
        .bind(&id)
        .bind(&payload.doc_path)
        .bind(requested_by)
        .bind(now)
        .bind(&payload_json)
        .execute(&mut *tx)
        .await;
        if let Err(e) = inserted {
            if is_unique_violation(&e) {
                return Err(DocsError::BadRequest(format!(
                    "a flush is already queued for {} — its payload carries these decisions",
                    payload.doc_path
                )));
            }
            return Err(storage(e));
        }

        // Delete ONLY the rows this payload actually snapshotted.
        //
        // This used to delete every pending row for the doc, while the
        // snapshot came from the caller's payload and the check above
        // only verified that SOME rows existed. A decision recorded
        // between the caller building its payload and this transaction
        // running was therefore deleted having never been captured
        // anywhere — silently, with the flush job looking complete and
        // internally consistent (feedback 1dd28e4c, defect 2).
        //
        // Deleting by anchor makes the race harmless instead: a
        // decision that arrived late is not in the payload, so it is
        // not deleted, and it is still pending for the next flush.
        let anchors: Vec<String> = payload.decisions.iter().map(|d| d.anchor.clone()).collect();
        sqlx::query(
            "DELETE FROM design_pending_decisions
             WHERE doc_path = $1 AND anchor = ANY($2)",
        )
        .bind(&payload.doc_path)
        .bind(&anchors)
        .execute(&mut *tx)
        .await
        .map_err(storage)?;

        // pending_count reflects what SURVIVES, not zero. With the
        // delete now scoped to the snapshot, a late-arriving decision
        // is still pending and the badge has to say so — otherwise the
        // row lives on with a count of 0 and nobody flushes it.
        sqlx::query(
            "UPDATE design_docs SET pending_count = (
                 SELECT COUNT(*) FROM design_pending_decisions WHERE doc_path = $1
             ) WHERE path = $1",
        )
        .bind(&payload.doc_path)
        .execute(&mut *tx)
        .await
        .map_err(storage)?;

        let row = sqlx::query("SELECT * FROM design_flush_jobs WHERE id = $1")
            .bind(&id)
            .fetch_one(&mut *tx)
            .await
            .map_err(storage)?;
        let job = job_from_row(&row)?;

        tx.commit().await.map_err(storage)?;
        Ok(job)
    }

    async fn flush_job_by_id(&self, id: &str) -> Result<Option<FlushJob>, DocsError> {
        let row = sqlx::query("SELECT * FROM design_flush_jobs WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map_err(storage)?;
        row.as_ref().map(job_from_row).transpose()
    }

    async fn flush_jobs_by_status(&self, status: JobStatus) -> Result<Vec<FlushJob>, DocsError> {
        let rows =
            sqlx::query("SELECT * FROM design_flush_jobs WHERE status = $1 ORDER BY queued_at")
                .bind(status.as_str())
                .fetch_all(&self.pool)
                .await
                .map_err(storage)?;
        rows.iter().map(job_from_row).collect()
    }

    async fn update_flush_job_status(
        &self,
        id: &str,
        update: &JobStatusUpdate,
        worked_by: Option<&str>,
    ) -> Result<FlushJob, DocsError> {
        let now = Utc::now();
        let (started_at, completed_at) = match update.status {
            JobStatus::Running => (Some(now), None),
            JobStatus::Succeeded | JobStatus::Failed => (None, Some(now)),
            JobStatus::Queued => (None, None),
        };

        // Use a CASE to preserve existing started_at when setting
        // terminal states, and to clear completion when going back
        // to queued.
        let row = sqlx::query(
            "UPDATE design_flush_jobs SET
                status = $2,
                started_at = CASE
                    WHEN $2 = 'running' AND started_at IS NULL THEN $3
                    WHEN $2 = 'queued' THEN started_at
                    ELSE started_at
                END,
                completed_at = CASE
                    WHEN $2 IN ('succeeded','failed') THEN $4
                    WHEN $2 = 'queued' THEN NULL
                    ELSE completed_at
                END,
                commit_sha = COALESCE($5, commit_sha),
                error = CASE
                    WHEN $2 = 'queued' THEN NULL
                    ELSE COALESCE($6, error)
                END,
                -- Who moved it. A requeue clears it: a job waiting to
                -- run has no worker, and keeping the last one would
                -- describe the past as the present (backlog c3cd3301).
                worked_by = CASE
                    WHEN $2 = 'queued' THEN NULL
                    ELSE COALESCE($7, worked_by)
                END
             WHERE id = $1
             RETURNING *",
        )
        .bind(id)
        .bind(update.status.as_str())
        .bind(started_at)
        .bind(completed_at)
        .bind(&update.commit_sha)
        .bind(&update.error)
        .bind(worked_by)
        .fetch_optional(&self.pool)
        .await
        .map_err(storage)?;

        match row {
            Some(r) => job_from_row(&r),
            None => Err(DocsError::NotFound(id.to_string())),
        }
    }

    async fn retry_flush_job(&self, id: &str) -> Result<FlushJob, DocsError> {
        // Read current status first; if already succeeded, it's a no-op.
        let existing = self.flush_job_by_id(id).await?;
        let Some(job) = existing else {
            return Err(DocsError::NotFound(id.to_string()));
        };
        if job.status == JobStatus::Succeeded {
            return Ok(job);
        }
        self.update_flush_job_status(
            id,
            &JobStatusUpdate {
                status: JobStatus::Queued,
                commit_sha: None,
                error: None,
            },
            // A requeue clears the worker; there is nobody to name.
            None,
        )
        .await
    }

    async fn recent_flush_jobs(&self, limit: i64) -> Result<Vec<FlushJob>, DocsError> {
        let rows = sqlx::query("SELECT * FROM design_flush_jobs ORDER BY queued_at DESC LIMIT $1")
            .bind(limit)
            .fetch_all(&self.pool)
            .await
            .map_err(storage)?;
        rows.iter().map(job_from_row).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{DecisionKind, DocStatus, FlushDecision};
    use boss_testing::TestDb;
    use chrono::TimeZone;

    fn sample_doc(path: &str) -> DesignDoc {
        DesignDoc {
            path: path.to_string(),
            title: "Test Doc".to_string(),
            status: DocStatus::InReview,
            pending_count: 0,
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

    /// The UNION in `upsert_doc` is the whole fix, and only a real
    /// database exercises it. Two answers, in the two different places
    /// an answer can be sitting when the file has not been rewritten
    /// yet: one still pending, one already snapshotted into a queued
    /// flush job. Neither doc heading says `(resolved)`.
    #[tokio::test(flavor = "multi_thread")]
    async fn recorded_answers_resolve_their_questions_wherever_they_sit() {
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
            repo.upsert_pending_decision(
                &PendingDecisionInput {
                    doc_path: "docs/design/a.md".to_string(),
                    anchor: anchor.to_string(),
                    kind: DecisionKind::Accept,
                    resolution: "WireGuard".to_string(),
                    rationale: None,
                },
                "david@algedonic.dev",
            )
            .await
            .unwrap();
        }
        // Q2's answer moves into a queued flush job, which deletes its
        // pending row. Q1's stays pending.
        repo.create_flush_job(
            &FlushJobPayload {
                doc_path: "docs/design/a.md".to_string(),
                base_commit_sha: "abc".to_string(),
                decisions: vec![FlushDecision {
                    anchor: "Q2".to_string(),
                    kind: DecisionKind::Accept,
                    resolution: "WireGuard".to_string(),
                    rationale: None,
                }],
            },
            "david@algedonic.dev",
        )
        .await
        .unwrap();

        repo.upsert_doc(&doc, &parsed()).await.unwrap();

        let qs = repo.questions_for_doc("docs/design/a.md").await.unwrap();
        let by_anchor = |a: &str| qs.iter().find(|q| q.anchor == a).unwrap().resolved;
        assert!(by_anchor("Q1"), "Q1 has a pending decision — answered");
        assert!(
            by_anchor("Q2"),
            "Q2's answer is in a queued flush — answered"
        );
        assert!(!by_anchor("Q3"), "Q3 was never answered — still open");
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

    #[tokio::test(flavor = "multi_thread")]
    async fn pending_decisions_overwrite_same_anchor() {
        let db = TestDb::new().await;
        let repo = PgDocsRepo::new(db.pool.clone());
        repo.upsert_doc(&sample_doc("docs/design/a.md"), &[])
            .await
            .unwrap();
        repo.upsert_pending_decision(
            &PendingDecisionInput {
                doc_path: "docs/design/a.md".to_string(),
                anchor: "Q1".to_string(),
                kind: DecisionKind::Accept,
                resolution: "first".to_string(),
                rationale: None,
            },
            "alice",
        )
        .await
        .unwrap();
        repo.upsert_pending_decision(
            &PendingDecisionInput {
                doc_path: "docs/design/a.md".to_string(),
                anchor: "Q1".to_string(),
                kind: DecisionKind::Override,
                resolution: "better".to_string(),
                rationale: Some("changed mind".to_string()),
            },
            "alice",
        )
        .await
        .unwrap();
        let pending = repo
            .pending_decisions_for_doc("docs/design/a.md")
            .await
            .unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].resolution, "better");
        assert_eq!(pending[0].kind, DecisionKind::Override);
        let doc = repo.doc_by_path("docs/design/a.md").await.unwrap().unwrap();
        assert_eq!(doc.pending_count, 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_decision_recorded_after_the_payload_survives_the_flush() {
        // Regression for feedback 1dd28e4c defect 2. The delete used to
        // take every pending row for the doc while the snapshot came
        // from the caller's payload, so a decision recorded in the gap
        // between payload-build and flush-create was destroyed without
        // ever being captured — silently, with the flush job looking
        // complete. Deleting by anchor makes that race harmless.
        let db = TestDb::new().await;
        let repo = PgDocsRepo::new(db.pool.clone());
        repo.upsert_doc(&sample_doc("docs/design/a.md"), &[])
            .await
            .unwrap();
        for i in 0..2 {
            repo.upsert_pending_decision(
                &PendingDecisionInput {
                    doc_path: "docs/design/a.md".to_string(),
                    anchor: format!("Q{i}"),
                    kind: DecisionKind::Accept,
                    resolution: format!("answer {i}"),
                    rationale: None,
                },
                "alice",
            )
            .await
            .unwrap();
        }

        // The caller snapshots Q0 and Q1...
        let payload = FlushJobPayload {
            doc_path: "docs/design/a.md".to_string(),
            base_commit_sha: "abc123".to_string(),
            decisions: (0..2)
                .map(|i| FlushDecision {
                    anchor: format!("Q{i}"),
                    kind: DecisionKind::Accept,
                    resolution: format!("answer {i}"),
                    rationale: None,
                })
                .collect(),
        };

        // ...and Q2 lands before the flush job is created.
        repo.upsert_pending_decision(
            &PendingDecisionInput {
                doc_path: "docs/design/a.md".to_string(),
                anchor: "Q2".to_string(),
                kind: DecisionKind::Accept,
                resolution: "the late one".to_string(),
                rationale: None,
            },
            "bob",
        )
        .await
        .unwrap();

        repo.create_flush_job(&payload, "alice").await.unwrap();

        // Q2 was never snapshotted, so it must still be pending.
        let pending = repo
            .pending_decisions_for_doc("docs/design/a.md")
            .await
            .unwrap();
        assert_eq!(
            pending.len(),
            1,
            "the un-snapshotted decision was destroyed by the flush: {pending:?}"
        );
        assert_eq!(pending[0].anchor, "Q2");
        assert_eq!(pending[0].resolution, "the late one");

        // And the badge must say there is still one to flush.
        let doc = repo.doc_by_path("docs/design/a.md").await.unwrap().unwrap();
        assert_eq!(
            doc.pending_count, 1,
            "pending_count was zeroed while a decision was still pending"
        );
    }

    /// A doc can have at most one flush WAITING, and the database is
    /// what guarantees it.
    ///
    /// David finished a review of eight questions on 2026-08-15 and
    /// could not find it anywhere. All eight had been recorded — eight
    /// POSTs, eight 200s — and the queue rule, which fires once per
    /// recorded decision, turned them into THREE identical jobs. The
    /// pending rows were consumed into those payloads, so the
    /// `pending_count` the page reads was 0. A finished review was
    /// indistinguishable from one that never registered.
    ///
    /// `post_flush_job` reads the pending rows OUTSIDE the transaction,
    /// so under a burst every firing arrives with the same payload and
    /// the defensive re-read inside the transaction still sees rows —
    /// a sibling that has not committed is invisible to it. No amount
    /// of checking fixes that; only a constraint does.
    ///
    /// Asserted against the index directly rather than by racing two
    /// calls. A first attempt at this test drove `create_flush_job`
    /// twice in sequence and passed with the migration REMOVED — the
    /// second call was rejected by the pre-existing "no pending
    /// decisions" path, so it proved nothing about the index. A test
    /// that cannot fail is worse than no test.
    #[tokio::test(flavor = "multi_thread")]
    async fn only_one_flush_may_wait_per_doc() {
        let db = TestDb::new().await;
        let repo = PgDocsRepo::new(db.pool.clone());
        repo.upsert_doc(&sample_doc("docs/design/a.md"), &[])
            .await
            .unwrap();

        let insert = |id: &'static str, status: &'static str| {
            let pool = db.pool.clone();
            async move {
                sqlx::query(
                    "INSERT INTO design_flush_jobs
                        (id, doc_path, status, requested_by, queued_at, payload)
                     VALUES ($1, 'docs/design/a.md', $2, 'rule', now(), $3::jsonb)",
                )
                .bind(id)
                .bind(status)
                .bind(
                    r#"{"doc_path":"docs/design/a.md","base_commit_sha":"0000000","decisions":[]}"#,
                )
                .execute(&pool)
                .await
            }
        };

        insert("fj-first", "queued").await.expect("the first waits");

        let second = insert("fj-second", "queued").await;
        let err = second.expect_err("a second waiting flush for one doc must be refused");
        assert!(
            is_unique_violation(&err),
            "expected the partial unique index to refuse it, got: {err}"
        );

        // History is unconstrained: a doc accumulates finished flushes
        // over its life, and only the WAITING one must be singular.
        insert("fj-done", "succeeded")
            .await
            .expect("a succeeded job alongside a queued one is ordinary history");
        insert("fj-dead", "failed")
            .await
            .expect("so is a failed one");

        let queued = repo.flush_jobs_by_status(JobStatus::Queued).await.unwrap();
        assert_eq!(
            queued
                .iter()
                .filter(|j| j.doc_path == "docs/design/a.md")
                .count(),
            1,
            "one doc, one waiting flush"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn create_flush_job_is_atomic() {
        let db = TestDb::new().await;
        let repo = PgDocsRepo::new(db.pool.clone());
        repo.upsert_doc(&sample_doc("docs/design/a.md"), &[])
            .await
            .unwrap();
        for i in 0..3 {
            repo.upsert_pending_decision(
                &PendingDecisionInput {
                    doc_path: "docs/design/a.md".to_string(),
                    anchor: format!("Q{i}"),
                    kind: DecisionKind::Accept,
                    resolution: format!("answer {i}"),
                    rationale: None,
                },
                "alice",
            )
            .await
            .unwrap();
        }
        let doc_before = repo.doc_by_path("docs/design/a.md").await.unwrap().unwrap();
        assert_eq!(doc_before.pending_count, 3);

        let payload = FlushJobPayload {
            doc_path: "docs/design/a.md".to_string(),
            base_commit_sha: "abc123".to_string(),
            decisions: (0..3)
                .map(|i| FlushDecision {
                    anchor: format!("Q{i}"),
                    kind: DecisionKind::Accept,
                    resolution: format!("answer {i}"),
                    rationale: None,
                })
                .collect(),
        };
        let job = repo.create_flush_job(&payload, "alice").await.unwrap();
        assert_eq!(job.status, JobStatus::Queued);
        assert_eq!(job.payload.decisions.len(), 3);

        // Pending rows cleared.
        let pending = repo
            .pending_decisions_for_doc("docs/design/a.md")
            .await
            .unwrap();
        assert!(pending.is_empty());
        let doc_after = repo.doc_by_path("docs/design/a.md").await.unwrap().unwrap();
        assert_eq!(doc_after.pending_count, 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn create_flush_job_with_no_pending_errors() {
        let db = TestDb::new().await;
        let repo = PgDocsRepo::new(db.pool.clone());
        repo.upsert_doc(&sample_doc("docs/design/a.md"), &[])
            .await
            .unwrap();
        let payload = FlushJobPayload {
            doc_path: "docs/design/a.md".to_string(),
            base_commit_sha: "abc123".to_string(),
            decisions: vec![],
        };
        let result = repo.create_flush_job(&payload, "alice").await;
        assert!(matches!(result, Err(DocsError::BadRequest(_))));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn update_flush_job_status_lifecycle() {
        let db = TestDb::new().await;
        let repo = PgDocsRepo::new(db.pool.clone());
        repo.upsert_doc(&sample_doc("docs/design/a.md"), &[])
            .await
            .unwrap();
        repo.upsert_pending_decision(
            &PendingDecisionInput {
                doc_path: "docs/design/a.md".to_string(),
                anchor: "Q1".to_string(),
                kind: DecisionKind::Accept,
                resolution: "ok".to_string(),
                rationale: None,
            },
            "alice",
        )
        .await
        .unwrap();
        let job = repo
            .create_flush_job(
                &FlushJobPayload {
                    doc_path: "docs/design/a.md".to_string(),
                    base_commit_sha: "abc".to_string(),
                    decisions: vec![FlushDecision {
                        anchor: "Q1".to_string(),
                        kind: DecisionKind::Accept,
                        resolution: "ok".to_string(),
                        rationale: None,
                    }],
                },
                "alice",
            )
            .await
            .unwrap();

        repo.update_flush_job_status(
            &job.id,
            &JobStatusUpdate {
                status: JobStatus::Running,
                commit_sha: None,
                error: None,
            },
            Some("emp-worker"),
        )
        .await
        .unwrap();
        let fetched = repo.flush_job_by_id(&job.id).await.unwrap().unwrap();
        assert_eq!(fetched.status, JobStatus::Running);
        assert!(fetched.started_at.is_some());

        repo.update_flush_job_status(
            &job.id,
            &JobStatusUpdate {
                status: JobStatus::Succeeded,
                commit_sha: Some("def456".to_string()),
                error: None,
            },
            Some("emp-worker"),
        )
        .await
        .unwrap();
        let fetched = repo.flush_job_by_id(&job.id).await.unwrap().unwrap();
        assert_eq!(fetched.status, JobStatus::Succeeded);
        assert_eq!(fetched.commit_sha.as_deref(), Some("def456"));
        assert!(fetched.completed_at.is_some());
        // The column the CLI's signed PUT lands in (backlog c3cd3301):
        // who ASKED and who MOVED it are different facts.
        assert_eq!(fetched.requested_by, "alice");
        assert_eq!(fetched.worked_by.as_deref(), Some("emp-worker"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn retry_failed_resets_to_queued() {
        let db = TestDb::new().await;
        let repo = PgDocsRepo::new(db.pool.clone());
        repo.upsert_doc(&sample_doc("docs/design/a.md"), &[])
            .await
            .unwrap();
        repo.upsert_pending_decision(
            &PendingDecisionInput {
                doc_path: "docs/design/a.md".to_string(),
                anchor: "Q1".to_string(),
                kind: DecisionKind::Accept,
                resolution: "ok".to_string(),
                rationale: None,
            },
            "alice",
        )
        .await
        .unwrap();
        let job = repo
            .create_flush_job(
                &FlushJobPayload {
                    doc_path: "docs/design/a.md".to_string(),
                    base_commit_sha: "abc".to_string(),
                    decisions: vec![FlushDecision {
                        anchor: "Q1".to_string(),
                        kind: DecisionKind::Accept,
                        resolution: "ok".to_string(),
                        rationale: None,
                    }],
                },
                "alice",
            )
            .await
            .unwrap();
        repo.update_flush_job_status(
            &job.id,
            &JobStatusUpdate {
                status: JobStatus::Failed,
                commit_sha: None,
                error: Some("boom".to_string()),
            },
            Some("emp-worker"),
        )
        .await
        .unwrap();

        let retried = repo.retry_flush_job(&job.id).await.unwrap();
        assert_eq!(retried.status, JobStatus::Queued);
        assert!(retried.error.is_none());
        assert!(retried.completed_at.is_none());
        // Requeued: waiting again, so nobody is working it.
        assert_eq!(retried.worked_by, None);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn retry_succeeded_is_noop() {
        let db = TestDb::new().await;
        let repo = PgDocsRepo::new(db.pool.clone());
        repo.upsert_doc(&sample_doc("docs/design/a.md"), &[])
            .await
            .unwrap();
        repo.upsert_pending_decision(
            &PendingDecisionInput {
                doc_path: "docs/design/a.md".to_string(),
                anchor: "Q1".to_string(),
                kind: DecisionKind::Accept,
                resolution: "ok".to_string(),
                rationale: None,
            },
            "alice",
        )
        .await
        .unwrap();
        let job = repo
            .create_flush_job(
                &FlushJobPayload {
                    doc_path: "docs/design/a.md".to_string(),
                    base_commit_sha: "abc".to_string(),
                    decisions: vec![FlushDecision {
                        anchor: "Q1".to_string(),
                        kind: DecisionKind::Accept,
                        resolution: "ok".to_string(),
                        rationale: None,
                    }],
                },
                "alice",
            )
            .await
            .unwrap();
        repo.update_flush_job_status(
            &job.id,
            &JobStatusUpdate {
                status: JobStatus::Succeeded,
                commit_sha: Some("def".to_string()),
                error: None,
            },
            Some("emp-worker"),
        )
        .await
        .unwrap();
        let retried = repo.retry_flush_job(&job.id).await.unwrap();
        assert_eq!(retried.status, JobStatus::Succeeded);
        assert_eq!(retried.commit_sha.as_deref(), Some("def"));
    }
}
