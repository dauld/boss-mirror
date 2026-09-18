-- 20260918063829-instance-data-leaves-the-platform-schema.sql — the
-- estate's nodes and one operator's credential ids stop riding every
-- fresh database (backlog ee368d0c; design 42277636 first wave, audit
-- H5, 2026-09-18).
--
-- MEASURED. Nine migrations in this directory INSERT instance data:
-- 144-estate-subjects and 202608301900 seed seven `nodes` on this LAN
-- (10.20.0.11-16, one public address) and eight `service_instances`;
-- 202609120300 and 202609121800 seed five `node_roles` rows (one later
-- removed by 20260914161807); 202609031700, 202609081230,
-- 20260916035349, 20260916040500 and 20260917041500 seed eight
-- `credentials` rows — the forge tokens, the machine token, the dev
-- session's ServiceAccount, the publish account's token, the tunnel
-- credentials and their account root, the Stripe read key. Every OSS
-- install applied all of them: an adopter's first boot declared a
-- cluster it does not run and credentials it does not hold, and the
-- eviction init.sh runs for the example rows (example-reference-rows.sh)
-- covered none of these tables. 20260916040500 re-declared
-- `cloudflare-tunnel-credentials` twelve minutes after 20260916035349
-- under ON CONFLICT (id) DO NOTHING, so its text — the broker-rotation
-- declaration — never landed on any database; the row every instance
-- carries is the earlier hand-mint declaration.
--
-- WHERE THE ROWS LIVE NOW. Credentials are the INSTANCE's declaration:
-- `seeds/credentials.toml` in the tenant contract, sent by `boss tenant
-- publish` through POST /api/credentials/batch (insert-if-absent by
-- id, one `credential.declared` fact per inserted row). The estate's
-- nodes and their roles are the tree's declaration: infra/estate/
-- estate.toml, published on every pod start by the launcher through
-- POST /api/estate/nodes/batch (the same door shape). Both doors keep
-- a row already there as it is, so an instance whose migrations
-- already seeded a row it declares sees `inserted 0` and no change.
--
-- WHAT THIS DOES. Deletes the seeded rows WHERE NOTHING REFERENCES
-- THEM, table by table in dependency order: a credential a sensor
-- names or a packet is about stays; a node a packet is about stays,
-- with its roles; a service instance stays only for a node that
-- stays. Every id below is one the migrations named, quoted, one per
-- line — the shape crates/core/boss-jobs/tests/
-- instance_data_leaves_the_platform_schema_pg.rs re-derives from this
-- directory's INSERT statements and holds equal in both directions.
-- On a fresh database this runs after the inserts and before any
-- declaration, so the first publish lands exactly what the instance
-- declares. On a converged database the declaration re-lands the rows
-- the instance still declares on its next pod start; a row it no
-- longer declares and nothing references is gone, which is the
-- retirement the registry never had a door for.
--
-- THE EXCEPTION THIS TAKES is the one 20260918022108 took for the
-- dispatcher rules: these rows are not a decision anyone made about
-- THIS instance — they are one estate's inventory and one operator's
-- book-keeping, written into the platform schema because no other door
-- existed. Deleting seed residue is not deleting history; the audit
-- log keeps every rotation and every observation recorded against
-- them, and the declaration that replaces them leaves its own fact.
--
-- The `node.declared` kind is declared here beside `credential.declared`
-- so neither rides inside a passing audit-integrity run unread
-- (infra/lint/emitted-kinds-are-declared.sh).

INSERT INTO event_kinds (kind_pattern, source, description, suffix_domain) VALUES
  ('credential.declared', 'jobs', 'An instance''s batch inserted one credentials registry row (POST /api/credentials/batch, insert-if-absent by id): the declaration as inserted — kind, issuer, principal, scopes, storage LOCATION, consumers, rotation policy; never a value — plus declared_by, the actor the request signed with, and tenant_id. One per inserted row, none for a kept row, none per batch', NULL),
  ('node.declared', 'jobs', 'The tree''s estate declaration inserted one nodes row and its node_roles (POST /api/estate/nodes/batch, insert-if-absent by id): the declaration as inserted plus declared_by, the actor the request signed with. One per inserted node, none for a kept one, none per batch', NULL)
ON CONFLICT (kind_pattern) DO NOTHING;

-- Credentials: kept while a sensor polls with it or a packet is about
-- it (a rotate-a-credential packet's subject is the credential id).
DELETE FROM credentials c
 WHERE NOT EXISTS (SELECT 1 FROM sensors s WHERE s.credential = c.id)
   AND NOT EXISTS (SELECT 1 FROM jobs j WHERE j.subject_id = c.id)
   AND c.id IN (
    'boss-credential-broker-root',
    'boss-dev-forge-token',
    'boss-machine-token',
    'cloudflare-account-token',
    'cloudflare-tunnel-credentials',
    'dauld-github-token',
    'dev-session-token',
    'stripe-restricted-read'
   );

-- Service instances: nothing in the tree reads the table (measured
-- 2026-09-18: no Rust, no shell, one hardcoded door in the SPA); a
-- row goes with its node unless a packet is about it.
DELETE FROM service_instances i
 WHERE NOT EXISTS (SELECT 1 FROM jobs j WHERE j.subject_kind = 'service-instance' AND j.subject_id = i.id)
   AND NOT EXISTS (SELECT 1 FROM jobs j WHERE j.subject_kind = 'node' AND j.subject_id = i.node_id)
   AND i.id IN (
    'boss-cluster',
    'boss-dev-0',
    'boss-dev-ssh',
    'boss-gcp-local',
    'forgejo',
    'gate-runner',
    'kanidm'
   );

-- Roles go with their node: a node a packet is about keeps its roles.
DELETE FROM node_roles r
 WHERE NOT EXISTS (SELECT 1 FROM jobs j WHERE j.subject_kind = 'node' AND j.subject_id = r.node_id)
   AND r.node_id IN (
    'boss-gcp',
    'cp-1',
    'cp-2',
    'cp-3',
    'forge',
    'w-1',
    'w-2'
   );

DELETE FROM nodes n
 WHERE NOT EXISTS (SELECT 1 FROM jobs j WHERE j.subject_kind = 'node' AND j.subject_id = n.id)
   AND NOT EXISTS (SELECT 1 FROM node_roles r WHERE r.node_id = n.id)
   AND NOT EXISTS (SELECT 1 FROM service_instances i WHERE i.node_id = n.id)
   AND n.id IN (
    'boss-gcp',
    'cp-1',
    'cp-2',
    'cp-3',
    'forge',
    'w-1',
    'w-2'
   );

-- The identity rows the migrations projected from the domain rows go
-- when the domain row is gone (144's own rule: the domain row owns
-- naming; `subjects` is the projection).
DELETE FROM subjects s
 WHERE s.kind = 'node'
   AND NOT EXISTS (SELECT 1 FROM nodes n WHERE n.id = s.id);

DELETE FROM subjects s
 WHERE s.kind = 'service-instance'
   AND NOT EXISTS (SELECT 1 FROM service_instances i WHERE i.id = s.id);
