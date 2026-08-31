-- 202608310030 — the dev session's ssh door joins the estate.
--
-- David decided the access shape on 2026-08-30 ("I just want to do the
-- ssh link", packet 02f4a2ee), revising dev-node-checkout/durable-session
-- Q2: alongside kubectl exec, boss-dev serves sshd (dropbear) behind
-- 10.20.0.35:22, key-only, David's ed25519. The estate registry records
-- the door the same session it opens, because the alternative is the
-- drift this registry exists to end: a service reachable on the LAN
-- that no declaration mentions.
--
-- Also corrects boss-dev-0's node: it was declared on cp-2 when seeded
-- (144), and the deployment has since moved to preferred-affinity on
-- the build node; the estate observer confirmed the pod on w-1 on
-- 2026-08-30. An UPDATE in a new migration, never an edit to 144 —
-- applied migrations are history (migrations-append-only).

INSERT INTO service_instances
    (id, label, service, node_id, environment, port, database_url, authoritative, leasable, notes)
VALUES
    ('boss-dev-ssh', 'Dev session ssh door', 'boss-dev-ssh', 'w-1', 'dev', 22, NULL, FALSE, FALSE,
     'Dropbear in the boss-dev pod, LoadBalancer 10.20.0.35:22, key-only auth (David''s ed25519, Secret boss-dev-ssh-authorized-keys by name). The ssh:// link answer to 02f4a2ee: macOS opens Terminal on ssh://root@10.20.0.35 natively. node_id tracks the deployment''s preferred affinity, not a hard pin — like boss-dev-0, it moves if the build node is cordoned.')
ON CONFLICT (id) DO NOTHING;

UPDATE service_instances
SET node_id = 'w-1'
WHERE id = 'boss-dev-0' AND node_id = 'cp-2';
