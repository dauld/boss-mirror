-- 202609030800-boarding-verifies-the-host.sql — the delivery policy
-- learns the CI host's disk floor.
--
-- On 2026-09-03 the conductor boarded two consists onto a CI host
-- whose disk was full; each burned a full CI cycle to discover it.
-- The locomotive's run-start `min_free_gb` check (default 70) even
-- PASSED at 01:26 and the test job still died mid-run — a run-start
-- floor cannot see the consist's mid-flight consumption. So boarding
-- now asks first: the conductor reads the CI host's latest host-scope
-- estate observation (`GET /api/estate/observations?scope=host`, the
-- series `infra/estate/observe-host.sh` posts) and refuses to assemble
-- a consist when observed `disk_free_gb` is under this floor.
--
-- 90 = the ~74GB a COLD workspace build measured on the forge host
-- (gate.sh's disk-floor note) plus headroom for mid-run growth — the
-- growth that walked through the locomotive's floor of 70. Like every
-- number on this table it is policy, editable as a registry version
-- bump; the DEFAULT below equals the conductor's compiled fallback
-- (`COMPILED_CI_HOST_FLOOR_GB`), and boss-cli's
-- `the_seeded_policy_equals_the_compiled_fallback` pins the two so
-- they cannot drift (CLAUDE.md §9a).
--
-- ADD COLUMN with a DEFAULT so the active v1 row — seeded by
-- 202608242117 without this column — carries the same floor the
-- compiled fallback does, and the change stays behaviour-neutral for
-- every existing reader.

ALTER TABLE delivery_policy
    ADD COLUMN IF NOT EXISTS ci_host_floor_gb INT NOT NULL DEFAULT 90
        CHECK (ci_host_floor_gb > 0);
