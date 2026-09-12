-- 202609112240 — cp-2's note stops saying the dev pod is pinned there.
--
-- The estate registry is where hardware questions are answered
-- ("never from memory or a doc"), and its note on cp-2 still read
-- "boss-dev is pinned here" from the 2026-08-16 seed (144). The dev
-- pod has PREFERRED the build node w-1 since its node affinity landed
-- (boss-dev.yaml: preferred, not required), and 202608310030 already
-- moved the boss-dev-0 service row to w-1 on 2026-08-30 — but the node
-- row kept the old sentence. It mattered on 2026-09-11 (incident
-- a398c4d3): naming which node held which workload during the cp-2 +
-- w-1 outage started from this row, and it said the opposite of the
-- placement the events showed.
--
-- The new note records what cp-2 actually carries: the SoR pod sits on
-- a control-plane node (boss.yaml nodeSelector) and was on cp-2 that
-- day, pods float; and the etcd member — the fact that made cp-2's
-- stall a cluster-wide one. An UPDATE in a new migration, never an
-- edit to 144: applied migrations are history (migrations-append-only).

UPDATE nodes
SET notes = 'Most cores of the three control-plane nodes, and an etcd member — a stall here is felt by every node whose KubePrism routes to its apiserver (incident a398c4d3, 2026-09-11). The SoR pod is control-plane-pinned (boss.yaml) and floats across cp-1/2/3; it sat here on 2026-09-11. The dev pod is NOT pinned here: it prefers the build node w-1 (boss-dev.yaml nodeAffinity), see 202608310030.'
WHERE id = 'cp-2'
  AND notes = 'Most cores of the three; boss-dev is pinned here.';
