-- 202609050510-boss-gcp-is-the-bastion.sql — the estate registry
-- catches up with the conductor cutover.
--
-- 144-estate-subjects.sql declared boss-gcp as role 'conductor' ("The
-- train conductor and the cadence loop"). On 2026-09-04 the conductor
-- moved into the cluster (feat/conductor-cutover, #198/#199; Deployment
-- boss-conductor in boss-dev), and the in-cluster conductor has run
-- every train since. The row kept saying conductor, so the units
-- observer on that host kept watching boss-train.service and raising
-- unit_unhealthy:boss-gcp/boss-train.service (alarm 72e410a5) on a
-- unit that is dead by design. docs/design/the-cluster-is-the-system
-- states the target: the cluster is the system, including the
-- conductor; boss-gcp is the bastion and public edge.
--
-- David approved the restatement on 2026-09-05 (approval fee6d286).
-- A seed row is edited by a later migration, never in place
-- (migrations-append-only); 144's insert stays as the record of what
-- was declared when.
UPDATE nodes
   SET role  = 'bastion',
       notes = 'WireGuard bastion and the public edge; hosts the second, demo BOSS stack (boss-gcp-local). The conductor moved into the cluster on 2026-09-04 (feat/conductor-cutover) — boss-train.service here is retired by design. 48GB is the smallest disk in the estate.'
 WHERE id = 'boss-gcp';
