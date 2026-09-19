-- 20260919011805-a-tenant-publish-is-a-fact-in-the-log.sql — a
-- successful `boss tenant publish` leaves one event in the audit log,
-- and the tenant_publishes row projects it.
--
-- Origin: backlog dbdc4d31, left by the builder of 6a8d4972 (instance
-- car 2, 2026-09-18). That car made the verb record a first-publish
-- stamp as a row in tenant_publishes (id, tenant_id, published_at,
-- published_by, boss_commit, took, writes), written through
-- BOSS_POSTGRES_URL so the launcher publishes once per database. The
-- row was the ONLY record: no audit_log event said "this database was
-- published from <boss_commit> by <actor>, taking <registries>". A
-- publish that writes N registry rows and a stamp without one fact in
-- the log is a provenance gap the rebuilder cannot see (CLAUDE.md:
-- every state-changing operation publishes an event; the log is the
-- system of record).
--
-- THE SHAPE. One `tenant.published` per stamp, payload = the stamp's
-- columns, `_actor` = published_by (one value, read once). Staged on
-- the transactional outbox in the SAME transaction as the row
-- (crates/orchestrators/boss-cli/src/tenant_stamp.rs, PgStamps::record
-- -> boss_events::outbox::record_event_in_tx), the door the ledger
-- verbs use; boss-event-relay lands it in audit_log post-commit. The
-- row stays as the launcher's fast read (`boss tenant published`).
--
-- Declared here so the kind never rides inside a passing
-- audit-integrity run unread (infra/lint/emitted-kinds-are-declared.sh
-- holds this row against the constant the verb emits).

INSERT INTO event_kinds (kind_pattern, source, description, suffix_domain) VALUES
  ('tenant.published', 'tenant', 'A tenant directory was published into this database (boss tenant publish, run by an operator or by the services launcher on a fresh instance): every door landed and the stamp row was written. Payload is the tenant_publishes row — tenant_id, published_at, published_by, boss_commit (the publishing binary''s build), took (the registries --take overwrote; empty for insert-if-absent), writes (doors written through) — with the actor as _actor', NULL)
ON CONFLICT (kind_pattern) DO NOTHING;
