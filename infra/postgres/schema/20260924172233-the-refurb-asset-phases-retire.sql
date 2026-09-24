-- 20260924172233-the-refurb-asset-phases-retire.sql — the four asset
-- lifecycle phases only the used-device shop's refurb pipeline could
-- reach retire with it.
--
-- WHY (one car of backlog a8991c86, the example-tenant retirement David
-- decided 2026-09-24). `triaging`, `refurbing`, `qa` and `ready` were
-- the phases the boss-assets projector set on TriageCompleted,
-- RefurbStarted, RefurbCompleted and QaPassed — events only the retiring
-- tenant's engine wrote. The same car deletes those four event kinds and
-- the four phase constants, so after it nothing can project an asset
-- into one of these phases. MEASURED 2026-09-24 through boss-api on the
-- system of record: the audit log holds 0 rows with source `assets`
-- (and 0 whose kind matches `asset.`, `refurb`, `triage` or
-- `qa_passed`) across all 459,629 rows (control: `step.` answered on
-- the same connection), and its equipment module is off. The playground
-- (the one example tenant) writes asset events only from its engine's
-- prepare — Received then Installed — so its assets sit at `installed`.
--
-- WHY A MIGRATION, AND WHY RETIRE RATHER THAN DELETE. 01-registries.sql
-- inserted the ten phase rows and migrations are append-only history,
-- so only a later migration can change them. `retired_at` is the Class
-- registry's own retirement (boss-classes lists and validates only rows
-- where it is NULL), so the rows stay as the record of what the
-- vocabulary once held. The NOT EXISTS guard keeps a phase that some
-- asset on some instance still sits in: a retirement must never strand
-- a projected row outside the active vocabulary. An UPDATE is not an
-- insert, so migrations-declare-schema-only.sh admits it.
-- boss-assets' projection_persistence.rs holds the rows this leaves
-- active equal to AssetLifecyclePhase::ORDER.

UPDATE classes c
   SET retired_at = NOW(),
       updated_at = NOW()
 WHERE c.subject_kind = 'asset'
   AND c.member_attribute = 'phase'
   AND c.code IN ('triaging', 'refurbing', 'qa', 'ready')
   AND c.retired_at IS NULL
   AND NOT EXISTS (SELECT 1 FROM assets a WHERE a.phase = c.code);
