-- 20260915221611-a-packet-carries-its-partition.sql — the packet
-- partition becomes a three-valued fact: real | simulated | shadow.
--
-- THE DECISION (packet 508cc38c; David's review of design 574c2adf,
-- 2026-08-22, Q2): the shadow lane of the experiments program
-- (docs/design/network-experiments.md, Tier 3) is a THIRD partition
-- value sharing the simulated lane's fail-closed machinery — not a
-- nested flag — so that fail-closed stays provable at every boundary
-- consumer: a consumer that refuses a simulated packet refuses a
-- shadow one the same way, and the sim never participates (Q5).
--
-- EXPAND, NOT CONTRACT. `jobs.simulated` (03-jobs.sql) stays, and every
-- write sets BOTH columns: `simulated` is DERIVED as `partition <>
-- 'real'`, so an N-1 reader of the bool fails closed on a shadow
-- packet without knowing the word. Reads of the partition come from
-- `partition`. Retiring the bool is a later car, after every reader
-- has moved — this car (1 of 4) moves the readers; nothing admits a
-- shadow packet until car 3, so the backfill below can only ever
-- produce 'real' and 'simulated'.
--
-- The rebuilder reproduces this column from the job-created event's
-- `_partition` marker, falling back to the older `_simulated` bool
-- (boss-jobs/src/rebuild.rs) — the same rule as the backfill.
ALTER TABLE jobs ADD COLUMN IF NOT EXISTS partition TEXT NOT NULL DEFAULT 'real';

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint WHERE conname = 'jobs_partition_check'
    ) THEN
        ALTER TABLE jobs ADD CONSTRAINT jobs_partition_check
            CHECK (partition IN ('real', 'simulated', 'shadow'));
    END IF;
END $$;

UPDATE jobs SET partition = 'simulated' WHERE simulated AND partition = 'real';

-- The trim's access pattern, by partition: every packet of one
-- non-real partition, by id (mirrors `jobs_simulated`, 03-jobs.sql).
CREATE INDEX IF NOT EXISTS jobs_partition ON jobs (partition) WHERE partition <> 'real';
