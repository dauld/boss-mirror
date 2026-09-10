-- 202609101900-the-corpus-index-is-deleted.sql — the READ half of the
-- design-doc markdown corpus, deleted. Part 2 of the deletion settled
-- in review 87f5bc84 on 2026-08-29 and recorded in
-- docs/architecture-decisions.md, "Design docs and the decision
-- record" (backlog f5da586c). Part 1 is
-- 202609101200-the-flush-pipeline-is-deleted.sql, earlier the same day.
--
-- THE DECISION. **The packet is the doc.** A `design-doc` packet carries
-- its own prose and its own questions as structured metadata, so
-- nothing needs to read a markdown file to find out what a doc asks.
-- Part 1 took the write-back half (the flush pipeline that edited the
-- file). This takes the read half: the index itself, the parser that
-- filled it, the `boss-docs-api` service that served it, and the two
-- dispatcher rules that spawned reviews from it.
--
-- NO HISTORY IS LOST. audit_log is the system of record and these are
-- projections and read-caches over git, rebuilt on every reindex by
-- definition. Every `docs.design.indexed` and
-- `docs.design.decision_recorded` event stays in the log, and both rows
-- in `event_kinds` are deliberately LEFT IN PLACE for the same reason
-- part 1 left one: the kinds WERE emitted, those audit rows exist, and
-- deleting a declaration would make boss-audit-integrity-check warn
-- about perfectly good history.
--
-- ======================================================================
-- 1. The closed ledger goes FIRST, because it only ever existed to
--    serve the reindex this migration is deleting.
-- ======================================================================
--
-- THE SEQUENCING RUNS THE OTHER WAY ROUND, and part 1 said so. It kept
-- `design_pending_decisions` alive — renamed `design_recorded_decisions`
-- and frozen as a ledger with no writers — because `upsert_doc` read it
-- on every reindex and force-resolved the anchors it names. Reindex
-- delete-and-reinserted the whole question set from the parse, so an
-- answer known only outside the file was erased on every boot, and
-- these rows were what put it back. Measured then: dropping it would
-- have re-opened 25 questions across 6 docs and handed David six fresh
-- review packets for questions he had already answered.
--
-- That reason is spent. With the parser, the reindexer and the service
-- gone, NOTHING will ever rebuild a question set again — so there is no
-- longer anything that could re-open those questions, and therefore
-- nothing for the ledger to hold shut. A table whose only reader is
-- deleted in the same migration is not an archive; audit_log holds the
-- `docs.design.decision_recorded` events, which is where the history
-- actually lives.
DROP TABLE IF EXISTS design_recorded_decisions;

-- ======================================================================
-- 2. The three read-caches. `design_questions` first: it carries the
--    FK to design_docs.
-- ======================================================================
--
-- `design_questions` — one row per `### Qn:` heading parsed out of a
-- file, re-derived by delete-and-insert on every reindex.
DROP TABLE IF EXISTS design_questions;

-- `design_doc_rejections` — docs that FAILED to parse, and why. Worth
-- naming what this one was for, because the defect it closed is real
-- and recurs: a rejected doc has no `design_docs` row by definition, so
-- without this table its absence looked exactly like "nobody wrote that
-- doc", and transactional-audit-log.md sat invisible for six days in
-- 2026-07 for a correctly-detected reason nobody could see. The lesson
-- (a refusal must be recorded where someone reads it, not returned in a
-- response body and discarded) outlives the table; there is nothing to
-- refuse now that nothing parses.
DROP TABLE IF EXISTS design_doc_rejections;

-- `design_docs` — one row per markdown file, with the rendered HTML.
-- Git was always the source of truth for the prose; this was the cache.
DROP TABLE IF EXISTS design_docs;

-- ======================================================================
-- 3. Retire the two dispatcher rules that read the corpus.
-- ======================================================================
--
-- `design-review-spawn` (107) fired on `docs.design.indexed` — an event
-- only the deleted reindexer emitted — and opened a `design-doc-review`
-- Job per doc with open questions. Inert the moment the indexer goes.
--
-- `design-review-level-sweep` (141) asked the same question on a daily
-- clock, through the `docs.design.sweep` handler, because the edge
-- version could only fire when a doc's parse CHANGED. Its handler read
-- `GET /api/design/docs`, which no longer exists.
--
-- RETIRED, NOT PORTED, and that is a decision rather than an omission.
-- Both rules answer "which DOC has open questions and no open review",
-- and a `design-doc` packet offers no way to ask it: the Workflow stamps
-- subject `{"custom","boss-platform"}` on every packet, so there is no
-- path key to join a review to a doc on; open-question counts live in
-- step metadata and are not stored anywhere countable; and the review is
-- a STEP OF THE PACKET rather than a separate Job to spawn. A packet
-- that needs reviewing is already in the `design-review` station's
-- queue the moment it is admitted — which is the whole point of the
-- packet being the doc, and leaves these two rules with nothing to add.
--
-- Retired rather than deleted: dispatcher_rules is an append-only
-- registry and a retired row is the record that the rule existed. Both
-- names, all versions — the loader reads status='active', and
-- infra/lint/the-live-rules-are-the-authored-rules.sh reads THIS write
-- to learn that the two files deleted from infra/dispatcher/rules/ were
-- retired on purpose rather than dropped by accident.
UPDATE dispatcher_rules SET status = 'retired'
 WHERE name IN ('design-review-spawn', 'design-review-level-sweep');

-- ======================================================================
-- 4. The queue is the DESIGN-DOC queue, which the live registry already
--    says and the tree's seed does not.
-- ======================================================================
--
-- 116-stations.sql seeds the `design-review` predicate as
-- `kind: design-doc-review` — the packets the deleted corpus page
-- opened, one per markdown file. The live cluster moved it to
-- `kind: design-doc` (station v3, authored through the API, no
-- migration), which is why the station's real queue holds `design-doc`
-- packets today. The tree never learned.
--
-- That drift was survivable while the page rendered files: the corpus
-- panel was the page whatever the predicate said. With the panel gone
-- the predicate IS the page, so a fresh database would render an empty
-- queue and look like "no design work", while the live one renders the
-- packets. Converging the tree to what live already enforces is the
-- smaller of the two changes available — the alternative is a surface
-- that behaves differently depending on which migrations a cluster has
-- taken.
--
-- UPDATE on the ACTIVE row, so it is a no-op where live has already
-- published v3 and a correction where it has not. Not a version bump:
-- a bump would insert a FOURTH version on a cluster that already has
-- three and make the tree the one that disagrees again.
UPDATE stations
   SET predicate = jsonb_set(predicate, '{kind}', '"design-doc"')
 WHERE name = 'design-review' AND status = 'active'
   AND predicate ->> 'kind' = 'design-doc-review';

-- ======================================================================
-- 5. The `/it/design` lens renders the queue, not the corpus.
-- ======================================================================
--
-- The `design-review` station's lens declared `panels: ["corpus"]`
-- (`["rejections", "corpus"]` in 138-station-lens.sql, which a fresh
-- database still applies). Both panels rendered FILES and both are
-- deleted; the surface now ships one panel, `queue`, which renders the
-- station's own packets.
--
-- THIS IS A FIX, not only a rename. The page fetched the queue already
-- and used it for exactly one thing: a join of `subject.id` to a doc
-- path. No `design-doc` packet can satisfy that join — its subject is
-- the literal `boss-platform` — so the map was always empty and the
-- station's real packets rendered nowhere, while a table of files
-- nobody had filed anything about rendered as the page. The lens that
-- said it was a view onto a queue is now one.
--
-- UPDATE on the ACTIVE row rather than a version bump, exactly as 119
-- and 138 did: this is platform-declared presentation on the platform's
-- own seed, in the same category as `title`. NOT guarded on
-- `lens IS NULL` — unlike 138, which was filling an empty column, this
-- must correct a populated one, and the live row was authored through
-- the API rather than by a migration (it reads `["corpus"]`, which no
-- file in this directory ever wrote). The subtitle is rewritten too
-- because the live one still offers "pending decisions", a thing part 1
-- deleted.
--
-- The bundle does not DEPEND on this running: `panelsFor` falls back to
-- the panels it ships when a row declares only keys it does not know,
-- so the window between deploy and converge renders the queue rather
-- than a header over blank space. This makes the registry say what is
-- true.
UPDATE stations
   SET lens = '{
         "eyebrow": "System Model · Design review",
         "title": "Design review",
         "subtitle": "Design docs waiting on a decision — the packet is the doc",
         "panels": ["queue"]
       }'::jsonb
 WHERE name = 'design-review' AND status = 'active';

-- `upstream` pointed at the corpus: `{"label": "DESIGN DOCS", "href":
-- "/system/design"}` (119-station-upstream.sql). It named where packets
-- in this queue come FROM, and they came from a human clicking a row in
-- the file table. They come from `boss design` now — a verb, not a
-- page — and the href was doubly dead anyway: `/system/*` was
-- consolidated into `/it/*` on 2026-08-31. A link to a page that does
-- not exist, labelled for a panel that does not exist, is worse than no
-- link.
UPDATE stations
   SET upstream = NULL
 WHERE name = 'design-review' AND status = 'active';
