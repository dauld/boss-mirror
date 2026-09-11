-- 202609050500-the-ci-host-floor-is-forty.sql — delivery policy v3:
-- ci_host_floor_gb 90 -> 40, so the CI-host boarding check can be
-- switched on without refusing every train.
--
-- David approved the proposal on 2026-09-05 (approval d99b198d,
-- "What disk floor should the CI-host boarding check use for the
-- forge?"). The 90 was the measured ~74GB cold FULL-gate build plus
-- headroom — a workload that now runs in-cluster with its own 160Gi
-- workspace. CI on the forge builds lean (fix/lean-ci-builds) and ran
-- green all week with 60-70GB free; the locomotive's own door refuses
-- under 70. A boarding floor of 40 fires only when the forge is
-- genuinely too tight for a lean CI build, and stays below the
-- locomotive's floor so boarding is the first to speak, not the last.
--
-- Append-only, versioned: retire the active row BEFORE inserting its
-- successor (registry-bump-retires-first — the partial unique index
-- on (name) WHERE status = 'active' is enforced per statement). v3 is
-- a copy of the row it retires with one column changed, so nothing
-- else about the policy moves; trains in flight stay pinned to the
-- version they departed under.
UPDATE delivery_policy
   SET status = 'retired'
 WHERE name = 'train-conductor' AND status = 'active';

INSERT INTO delivery_policy (
    name, version, status,
    max_red_trains, stall_hours,
    consist_excluded_lints,
    consist_budget_secs, consist_output_budget, consist_files_named,
    skip_reason_file_budget, blip_cause_budget,
    gate_max_concurrent, ci_host_floor_gb
)
SELECT name, version + 1, 'active',
       max_red_trains, stall_hours,
       consist_excluded_lints,
       consist_budget_secs, consist_output_budget, consist_files_named,
       skip_reason_file_budget, blip_cause_budget,
       gate_max_concurrent, 40
  FROM delivery_policy
 WHERE name = 'train-conductor'
 ORDER BY version DESC
 LIMIT 1;
