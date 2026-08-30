-- The estate registry knows the workers.
--
-- `144-estate-subjects.sql` declared BOSS's own substrate as Subjects
-- and seeded five nodes: cp-1, cp-2, cp-3, forge, boss-gcp. The cluster
-- has since grown two WORKERS, and the registry never learned:
--
--   w-1  10.20.0.14  joined 2026-08-25  boss.dev/purpose=build
--   w-2  10.20.0.16  joined 2026-08-25  boss.dev/purpose=utility
--
-- w-1 is not a minor omission. It is the BUILD NODE — 32 cores, the
-- largest disk in the estate, and the node every gate Job prefers
-- through `boss.dev/purpose=build` node affinity. The registry did not
-- know about the machine that compiles this repository.
--
-- MEASURED 2026-08-30 (David: "We have more than 4 machines. That is
-- out of date. We have learned that we need to check the current state
-- and not rely on memories"): seven managed machines running, five
-- declared. Both missing rows are workers. Three separate accounts of
-- the estate — this registry, a prose inventory written that afternoon,
-- and an operator's recollection — were wrong in the SAME direction,
-- because none of them was connected to the machines.
--
-- CAPACITY IS DECLARED, from `kubectl get nodes` capacity on
-- 2026-08-30, converted the way the existing rows were: memory rounded
-- to GiB, disk floored. It is DECLARED capacity, not observed state —
-- free space now is a measurement with a timestamp and belongs on the
-- log, which is what the `node` subject kind's own description says.
--
-- THIS FIXES THE DATA, NOT THE DRIFT. Nothing still reconciles the
-- registry against the cluster, so the next node to join will be
-- missing too. The protocol half — a cadence that enumerates live
-- nodes, compares them to this table, and OPENS A PACKET when they
-- disagree — is filed as 59ef456a. David, 2026-08-30: "We should be
-- improving our hardware management protocols just like our build and
-- deployment protocols. The physical realities introduce a lot of
-- complexity we need to incorporate in our protocols by continuing to
-- test and iterate with data."

INSERT INTO nodes (id, label, address, role, cpu, memory_gb, disk_gb, notes) VALUES
    ('w-1', 'w-1', '10.20.0.14', 'talos-worker', 32, 63, 928,
     'THE BUILD NODE (boss.dev/purpose=build). Every gate Job prefers it by node affinity — preferred, not required, so cordoning it degrades gating rather than stopping it. Most cores and the largest disk in the estate; a cold workspace build needs ~74GB of target/, and two branches'' targets do not fit on one gate volume. Deliberately not a control plane, so a gate cannot starve etcd.'),
    ('w-2', 'w-2', '10.20.0.16', 'talos-worker', 4,  15,  109,
     'Utility worker (boss.dev/purpose=utility). Joined 2026-08-25 and went unrecorded here for five days. Smallest node in the estate after boss-gcp.')
ON CONFLICT (id) DO NOTHING;

-- Identity rows, so a Job may name either worker as its subject —
-- the same pattern 144 uses for the original five.
INSERT INTO subjects (kind, id, label)
    SELECT 'node', id, label FROM nodes WHERE id IN ('w-1', 'w-2')
ON CONFLICT (kind, id) DO NOTHING;

-- The gate runner is a real service-instance and was never recorded.
-- It is where the pipeline's compute actually happens, and it is
-- leasable in the sense that matters: one gate at a time owns the disk.
INSERT INTO service_instances (id, label, service, node_id, environment, port, database_url, authoritative, leasable, notes) VALUES
    ('gate-runner', 'Gate runner', 'gate-runner', 'w-1', 'dev', NULL, 'sidecar postgres 16 on 127.0.0.1', FALSE, FALSE,
     'One Job per gate in namespace boss-dev: its own shallow clone, its own 120Gi longhorn-dev-disposable volume (gate-runner-disk), a Postgres native sidecar, and its script mounted from ConfigMap gate-runner-script. Jobs carry boss.dev/packet=<uuid>, which is how a packet''s Job is found without guessing. Prefers w-1 by affinity.')
ON CONFLICT (id) DO NOTHING;

INSERT INTO subjects (kind, id, label)
    SELECT 'service-instance', id, label FROM service_instances WHERE id = 'gate-runner'
ON CONFLICT (kind, id) DO NOTHING;
