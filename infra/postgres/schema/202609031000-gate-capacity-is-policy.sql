-- 202609031000-gate-capacity-is-policy.sql — the gate concurrency
-- limit becomes registry data, so one number is both what `boss gate`
-- enforces and what the yard draws as slots.
--
-- WHAT WAS FOLKLORE. `boss gate` admitted a fixed number of parallel
-- gates from `DEFAULT_MAX_CONCURRENT: usize = 3` — a private const in
-- `crates/orchestrators/boss-cli/src/gate.rs`, overridable only by the
-- BOSS_GATE_MAX_CONCURRENT env var. Nothing else could see it: the
-- Train Yard could not draw the gate capacity because the capacity was
-- compiled, and an operator could not raise it from 3 to 4 without a
-- code car. Now it is a policy row an operator edits, and both readers
-- take it from here.
--
-- The gate CLI reads it as: BOSS_GATE_MAX_CONCURRENT env (override,
-- kept) > this policy value > the compiled fallback 3. The yard status
-- read-model reads it to size its gate-slot visual. One source, so the
-- number a page shows is the number the pipeline obeys (CLAUDE.md §9a).
--
-- 3 = the measured comfort zone on the build node (w-1): at FIVE
-- concurrent gates I/O pressure sat at 65% and a ~35-minute gate took
-- ~93, so per-verdict latency degraded past two gates' worth of
-- queueing even though total throughput still beat serial. Like every
-- number on this table it is policy, editable as a registry version
-- bump; the DEFAULT below equals the CLI's compiled fallback
-- (`COMPILED_GATE_MAX_CONCURRENT`), and boss-cli's
-- `the_seeded_policy_equals_the_compiled_fallback` pins the two so they
-- cannot drift.
--
-- ADD COLUMN with a DEFAULT so the active v1 row — seeded by 202608242117
-- without this column — carries the same bound the compiled fallback
-- does, and the change stays behaviour-neutral for every existing
-- reader.

ALTER TABLE delivery_policy
    ADD COLUMN IF NOT EXISTS gate_max_concurrent INT NOT NULL DEFAULT 3
        CHECK (gate_max_concurrent > 0);
