//! `docs.design.sweep` — ask the level question on a clock.
//!
//! THE BUG THIS EXISTS FOR. `design-review-spawn` asks a LEVEL
//! question — "this doc has open questions and no open review" — but
//! only ever gets asked on an EDGE: `docs.design.indexed`, which by
//! design fires only when a doc's review surface changes. Close a
//! review while its questions are still open and the doc can never
//! get another one, because nothing about it will change again on its
//! own.
//!
//! That is not hypothetical. The 2026-08-13 audit closed five reviews
//! on the evidence "questions resolved; tracker pending_count=0;
//! decision history in doc" — and the evidence was wrong, because
//! that count (since deleted) read 0 when nobody had answered, and two of the
//! docs' Decision-history sections read `_None yet._`. Roughly
//! twenty-three questions across payload-encryption, queue-visibility,
//! workflow-ux-as-data, department-flow-dashboards and dev-cluster
//! became unreachable: not in anyone's queue, and with no mechanism
//! that could ever put them there. Confirmed directly — a reindex over
//! all 38 docs spawned ZERO reviews, because no doc's surface had
//! changed. They came back only because an agent went looking by hand
//! (ae8a14f7).
//!
//! WHY A SWEEP RATHER THAN A LOUDER EDGE. Making the reindex emit for
//! unchanged docs was considered and rejected: it turns every boot into
//! ~38 events, buys prompt spawning at the cost of a noisy log, and
//! STILL leaves the level unchecked between boots. The edge stays an
//! optimisation — spawn promptly on change — and this makes it stop
//! being the only path.
//!
//! WHY IT DELEGATES RATHER THAN SPAWNS. The obvious implementation
//! POSTs a Job itself, and then the shape of a design review lives in
//! two places: this file and the `design-review-spawn` rule row. So
//! instead the sweep decides only WHICH docs are orphaned and hands
//! each one to `jobs.spawn` as the payload the edge rule would have
//! delivered. The spawn spec stays where it belongs — in registry
//! rows an operator can read and edit — and this handler owns nothing
//! but the level question.
//!
//! THE WIDER CLASS, worth carrying: any dispatcher rule whose `when`
//! reads like a STATE rather than a TRANSITION has this latent.
//! Migration 107's own comment names `open_restock_exists` as the same
//! shape.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;

use boss_dispatcher::rules::expr::Value;
use boss_dispatcher::rules::handler::{Handler, HandlerError, InvocationContext};
use boss_dispatcher::rules::jobs_spawn::JobsSpawn;

use super::common::{dispatcher_actor_header, sim_origin_value};

pub struct DocsDesignSweep {
    client: reqwest::Client,
    docs_base: String,
    jobs_base: String,
    spawn: Arc<JobsSpawn>,
}

impl DocsDesignSweep {
    pub fn new(docs_base: impl Into<String>, jobs_base: impl Into<String>) -> Arc<Self> {
        let client = reqwest::Client::new();
        let jobs_base = jobs_base.into();
        Arc::new(Self {
            spawn: JobsSpawn::with_client(client.clone(), jobs_base.clone()),
            client,
            docs_base: docs_base.into(),
            jobs_base,
        })
    }

    pub fn with_client(
        client: reqwest::Client,
        docs_base: impl Into<String>,
        jobs_base: impl Into<String>,
    ) -> Arc<Self> {
        let jobs_base = jobs_base.into();
        Arc::new(Self {
            spawn: JobsSpawn::with_client(client.clone(), jobs_base.clone()),
            client,
            docs_base: docs_base.into(),
            jobs_base,
        })
    }

    /// Reads carry provenance too. The dispatcher-actor-stamp lint is
    /// right to insist: an unstamped read is one nobody can attribute
    /// when they are working out which rule hammered an API, and the
    /// sim-origin header is what keeps simulated traffic out of real
    /// projections.
    async fn get_json(&self, url: &str, rule: &str) -> Result<serde_json::Value, HandlerError> {
        let resp = self
            .client
            .get(url)
            .header("x-boss-user", dispatcher_actor_header(rule))
            .header("x-sim-origin", sim_origin_value())
            .send()
            .await
            .map_err(|e| HandlerError::Downstream(format!("GET {url}: {e}")))?;
        if !resp.status().is_success() {
            let status = resp.status();
            return Err(HandlerError::Downstream(format!(
                "GET {url} returned {status}"
            )));
        }
        resp.json()
            .await
            .map_err(|e| HandlerError::Downstream(format!("GET {url} body: {e}")))
    }
}

/// Docs carrying open questions, as `(path, title, open_questions)`.
///
/// `open_questions` and nothing else on the row. The list used to
/// carry a second count, `pending_count` ("answers recorded but not
/// written back"), and confusing the two is what made the 2026-08-13
/// audit close five live reviews — so this function reads exactly one
/// field and ignores whatever else a row carries. The count itself was
/// dropped with the flush pipeline on 2026-09-10; the discipline of
/// reading one named field stays.
pub(crate) fn docs_with_open_questions(list: &serde_json::Value) -> Vec<(String, String, i64)> {
    list.as_array()
        .map(|rows| {
            rows.iter()
                .filter_map(|r| {
                    let open = r.get("open_questions").and_then(|v| v.as_i64())?;
                    if open <= 0 {
                        return None;
                    }
                    let path = r.get("path")?.as_str()?.to_string();
                    let title = r
                        .get("title")
                        .and_then(|v| v.as_str())
                        .unwrap_or(&path)
                        .to_string();
                    Some((path, title, open))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The doc paths that already have an open review — the same question
/// `open_review_exists(path)` answers for the edge rule, asked through
/// the jobs API instead of the expr helper.
///
/// A review's subject IS the doc path (migration 107 spawns with
/// `subject: path`), so this compares subject ids and does not need to
/// understand the review Workflow at all.
pub(crate) fn paths_with_open_review(jobs: &serde_json::Value) -> Vec<String> {
    let rows = jobs
        .get("data")
        .and_then(|v| v.as_array())
        .or_else(|| jobs.as_array());
    rows.map(|rows| {
        rows.iter()
            .filter_map(|j| {
                j.get("subject")
                    .and_then(|s| s.get("id"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
            })
            .collect()
    })
    .unwrap_or_default()
}

#[async_trait]
impl Handler for DocsDesignSweep {
    fn name(&self) -> &'static str {
        "docs.design.sweep"
    }

    async fn invoke(
        &self,
        args: &[(String, Value)],
        ctx: &InvocationContext,
    ) -> Result<(), HandlerError> {
        let docs = self
            .get_json(
                &format!("{}/api/design/docs", self.docs_base.trim_end_matches('/')),
                &ctx.rule_name,
            )
            .await?;
        let candidates = docs_with_open_questions(&docs);
        if candidates.is_empty() {
            return Ok(());
        }

        // One read for every open review, rather than one per doc. The
        // corpus is ~38 files and the reviews are fewer; a query per
        // candidate would be 38 round trips to answer a question one
        // answers.
        let reviews = self
            .get_json(
                &format!(
                    "{}/api/jobs?kind=design-doc-review&status=open&limit=500",
                    self.jobs_base.trim_end_matches('/')
                ),
                &ctx.rule_name,
            )
            .await?;
        let covered = paths_with_open_review(&reviews);

        let mut spawned = 0usize;
        for (path, title, open) in candidates {
            if covered.iter().any(|c| c == &path) {
                continue;
            }
            // The payload the EDGE would have delivered, so `jobs.spawn`
            // binds exactly the identifiers migration 107's args name.
            // Shape copied from the emit site in boss-docs' upsert_doc.
            let synthetic = InvocationContext {
                rule_name: ctx.rule_name.clone(),
                triggering_event_id: ctx.triggering_event_id.clone(),
                triggering_topic: "docs.design.indexed".to_string(),
                event_payload: json!({
                    "path": path,
                    "title": title,
                    "open_questions": open,
                }),
            };
            self.spawn.invoke(args, &synthetic).await?;
            spawned += 1;
            tracing::info!(
                rule = %ctx.rule_name, doc = %path, open_questions = open,
                "sweep spawned a review the edge could not"
            );
        }
        if spawned > 0 {
            tracing::info!(rule = %ctx.rule_name, spawned, "design-review sweep");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_docs_with_unanswered_questions_are_candidates() {
        let list = json!([
            {"path": "docs/design/a.md", "title": "A", "open_questions": 3},
            {"path": "docs/design/b.md", "title": "B", "open_questions": 0},
        ]);
        assert_eq!(
            docs_with_open_questions(&list),
            vec![("docs/design/a.md".to_string(), "A".to_string(), 3)]
        );
    }

    // Only `open_questions` decides. A row carrying some OTHER count
    // must not be read as one — which is exactly how the 2026-08-13
    // audit closed five live reviews, reading the then-present
    // `pending_count` as the signal.
    #[test]
    fn no_other_count_on_the_row_is_mistaken_for_open_questions() {
        let list = json!([
            {"path": "docs/design/a.md", "title": "A", "open_questions": 0, "word_count": 4000},
        ]);
        assert!(docs_with_open_questions(&list).is_empty());
    }

    #[test]
    fn a_doc_without_a_title_falls_back_to_its_path() {
        let list = json!([{"path": "docs/design/a.md", "open_questions": 1}]);
        let got = docs_with_open_questions(&list);
        assert_eq!(got[0].1, "docs/design/a.md");
    }

    #[test]
    fn open_reviews_are_read_off_their_subject_id() {
        let jobs = json!({"data": [
            {"id": "j1", "subject": {"subject_kind": "custom", "id": "docs/design/a.md"}},
            {"id": "j2", "subject": {"subject_kind": "custom", "id": "docs/design/c.md"}},
        ]});
        assert_eq!(
            paths_with_open_review(&jobs),
            vec![
                "docs/design/a.md".to_string(),
                "docs/design/c.md".to_string()
            ]
        );
    }

    // The jobs API answers `{data: [...]}`; a bare array is accepted so
    // a shape change does not silently make every doc look uncovered —
    // which would spawn a duplicate review for the whole corpus.
    #[test]
    fn a_bare_array_of_jobs_is_also_understood() {
        let jobs = json!([{"subject": {"id": "docs/design/a.md"}}]);
        assert_eq!(
            paths_with_open_review(&jobs),
            vec!["docs/design/a.md".to_string()]
        );
    }

    // An empty or unrecognised body must yield NO coverage claims —
    // but note that means "spawn for everything", so the read failing
    // loudly (get_json errors on non-2xx) is what keeps this safe.
    #[test]
    fn an_unrecognised_body_claims_no_coverage() {
        assert!(paths_with_open_review(&json!({"error": "nope"})).is_empty());
    }
}
