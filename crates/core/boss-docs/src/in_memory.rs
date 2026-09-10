//! In-memory adapter. Used in tests and as a fallback when the
//! service is started without `--features postgres`.

use std::collections::{HashMap, HashSet};
use std::sync::RwLock;

use async_trait::async_trait;
use chrono::Utc;

use crate::port::{DocsError, DocsRepository};
use crate::types::{DesignDoc, DesignQuestion, RejectedDocRecord, apply_recorded_decisions};

#[derive(Default)]
struct Inner {
    docs: HashMap<String, DesignDoc>,
    questions: HashMap<String, Vec<DesignQuestion>>, // keyed by doc_path
    /// Anchors carrying a recorded decision, keyed by doc_path — the
    /// in-memory twin of `design_recorded_decisions`.
    recorded: HashMap<String, HashSet<String>>,
    rejections: HashMap<String, RejectedDocRecord>, // keyed by path
}

pub struct InMemoryDocsRepo {
    inner: RwLock<Inner>,
}

impl Default for InMemoryDocsRepo {
    fn default() -> Self {
        Self {
            inner: RwLock::new(Inner::default()),
        }
    }
}

impl InMemoryDocsRepo {
    pub fn new() -> Self {
        Self::default()
    }

    /// Seed a recorded decision for `(doc_path, anchor)`.
    ///
    /// The recorded-decision ledger is READ-ONLY in production — the
    /// flush pipeline that wrote it was deleted on 2026-09-10 and
    /// nothing writes to it since, so it is not a port method. This is
    /// the in-memory adapter's way to stand a row up so the
    /// `apply_recorded_decisions` merge is exercised against the same
    /// function the Pg adapter calls (CLAUDE.md §9a — one definition
    /// of the rule, not one per adapter).
    pub fn record_decision(&self, doc_path: &str, anchor: &str) {
        self.inner
            .write()
            .expect("docs lock")
            .recorded
            .entry(doc_path.to_string())
            .or_default()
            .insert(anchor.to_string());
    }
}

#[async_trait]
impl DocsRepository for InMemoryDocsRepo {
    async fn all_docs(&self) -> Result<Vec<DesignDoc>, DocsError> {
        let inner = self.inner.read().unwrap();
        let mut docs: Vec<_> = inner.docs.values().cloned().collect();
        docs.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(docs)
    }

    async fn doc_by_path(&self, path: &str) -> Result<Option<DesignDoc>, DocsError> {
        Ok(self.inner.read().unwrap().docs.get(path).cloned())
    }

    async fn questions_for_doc(&self, path: &str) -> Result<Vec<DesignQuestion>, DocsError> {
        Ok(self
            .inner
            .read()
            .unwrap()
            .questions
            .get(path)
            .cloned()
            .unwrap_or_default())
    }

    async fn upsert_doc(
        &self,
        doc: &DesignDoc,
        questions: &[DesignQuestion],
    ) -> Result<(), DocsError> {
        let mut inner = self.inner.write().unwrap();
        // Same merge the postgres adapter does — the packet decides.
        // Both call one function so the rule cannot drift between the
        // adapter under test and the adapter in production (§9a).
        let decided: HashSet<String> = inner.recorded.get(&doc.path).cloned().unwrap_or_default();
        let questions = apply_recorded_decisions(questions, &decided);
        inner.docs.insert(doc.path.clone(), doc.clone());
        inner.questions.insert(doc.path.clone(), questions);
        Ok(())
    }

    async fn all_rejections(&self) -> Result<Vec<RejectedDocRecord>, DocsError> {
        let g = self.inner.read().expect("docs lock");
        let mut out: Vec<RejectedDocRecord> = g.rejections.values().cloned().collect();
        out.sort_by_key(|r| r.first_seen_at);
        Ok(out)
    }

    async fn upsert_rejection(&self, path: &str, reason: &str) -> Result<(), DocsError> {
        let mut g = self.inner.write().expect("docs lock");
        let now = Utc::now();
        g.rejections
            .entry(path.to_string())
            .and_modify(|r| {
                r.reason = reason.to_string();
                r.last_seen_at = now;
            })
            .or_insert_with(|| RejectedDocRecord {
                path: path.to_string(),
                reason: reason.to_string(),
                first_seen_at: now,
                last_seen_at: now,
            });
        Ok(())
    }

    async fn clear_rejection(&self, path: &str) -> Result<(), DocsError> {
        self.inner
            .write()
            .expect("docs lock")
            .rejections
            .remove(path);
        Ok(())
    }

    async fn delete_doc(&self, path: &str) -> Result<(), DocsError> {
        let mut inner = self.inner.write().unwrap();
        inner.docs.remove(path);
        inner.questions.remove(path);
        inner.recorded.remove(path);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::DocStatus;
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

    #[tokio::test]
    async fn upsert_and_list_docs() {
        let repo = InMemoryDocsRepo::new();
        repo.upsert_doc(&sample_doc("docs/design/a.md"), &[])
            .await
            .unwrap();
        repo.upsert_doc(&sample_doc("docs/design/b.md"), &[])
            .await
            .unwrap();
        let docs = repo.all_docs().await.unwrap();
        assert_eq!(docs.len(), 2);
        assert_eq!(docs[0].path, "docs/design/a.md");
        assert_eq!(docs[1].path, "docs/design/b.md");
    }

    /// David, 2026-08-18: "I keep seeing jobs that I have responded
    /// to." This is that bug, at the port — and since the flush
    /// pipeline was deleted on 2026-09-10 the ledger is the ONLY thing
    /// standing between an answered question and a fresh review packet
    /// for it, because nothing will ever write `(resolved)` into the
    /// heading again.
    #[tokio::test]
    async fn an_answered_question_stops_counting_as_open() {
        let repo = InMemoryDocsRepo::new();
        let doc = sample_doc("docs/design/a.md");
        repo.upsert_doc(&doc, &[sample_question("docs/design/a.md", "Q1", 0)])
            .await
            .unwrap();

        repo.record_decision("docs/design/a.md", "Q1");

        // Reindex. The markdown is UNCHANGED — nothing wrote
        // `(resolved)` into the heading — so the parser still hands us
        // an open question, exactly as it does in production.
        repo.upsert_doc(&doc, &[sample_question("docs/design/a.md", "Q1", 0)])
            .await
            .unwrap();

        let qs = repo.questions_for_doc("docs/design/a.md").await.unwrap();
        assert!(
            qs[0].resolved,
            "he answered Q1; a reindex must not resurrect it as open, or \
             design-review-spawn hands him the same question again"
        );
    }

    #[tokio::test]
    async fn an_unanswered_question_stays_open() {
        let repo = InMemoryDocsRepo::new();
        let doc = sample_doc("docs/design/a.md");
        repo.record_decision("docs/design/a.md", "Q2");
        repo.upsert_doc(&doc, &[sample_question("docs/design/a.md", "Q1", 0)])
            .await
            .unwrap();
        let qs = repo.questions_for_doc("docs/design/a.md").await.unwrap();
        assert!(!qs[0].resolved, "Q1 has no recorded decision");
    }

    #[tokio::test]
    async fn upsert_doc_replaces_questions() {
        let repo = InMemoryDocsRepo::new();
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
        // Reindex with one question.
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
