-- 20260917071313-a-tenants-declarations-leave-a-fact.sql — the three
-- tenant batch doors record what they inserted.
--
-- Origin: backlog d9409039. Measured 2026-09-17 by the builders of
-- 1ec8312a and f56155f0: POST /api/classes/batch, /api/locations/batch
-- and /api/agents/batch each inserted rows and published NO event, so
-- a tenant's declarations — its classes, its locations, its agents —
-- left no audit-log fact, against "every state-changing operation
-- publishes an event" (CLAUDE.md §Events), and the rebuilders could
-- not reproduce them from the log. The classes port even said so in
-- its doc ("goes straight to the table without emitting an event"),
-- on the reasoning that a reference table the rebuilders never touch
-- is exempt. It is not: the question a tenant's row answers later is
-- "when did this appear, and who put it there", and only the log can.
--
-- ONE FACT PER INSERTED ROW. The doors are insert-if-absent, so a
-- re-run inserts nothing and records nothing — a kept row changed no
-- state. Never per batch: the rebuilders reproduce rows, not requests.
-- The payload is the row as inserted plus `declared_by`, the actor the
-- request signed with (the id `boss tenant publish` sends), read from
-- the same stamp `_actor` is read from so the two cannot disagree.
--
-- STAGED ON THE OUTBOX, in the insert's own transaction (the
-- credentials and calendar shape): the row and its fact commit or
-- roll back together, and boss-event-relay moves the fact to
-- audit_log and the bus. That is why boss-classes-api and
-- boss-locations-api need no NATS of their own — their configs stay
-- postgres_url + http_bind.
--
-- Declared here so the kinds do not ride inside a passing
-- audit-integrity run unread (infra/lint/emitted-kinds-are-declared.sh).

INSERT INTO event_kinds (kind_pattern, source, description, suffix_domain) VALUES
  ('class.declared', 'classes', 'A tenant''s batch inserted one Class row (POST /api/classes/batch, insert-if-absent): the row as inserted plus declared_by, the actor the request signed with. One per inserted row, none for a kept row, none per batch', NULL),
  ('location.declared', 'locations', 'A tenant''s batch inserted one Location row (POST /api/locations/batch, insert-if-absent): the row as inserted plus declared_by, the actor the request signed with. One per inserted row, none for a kept row, none per batch', NULL),
  ('agent.declared', 'jobs', 'A tenant''s batch inserted one agent row (POST /api/agents/batch, insert-if-absent by id): the declaration as inserted — id, display name, default model, caps, the aliases landed with it — plus declared_by, the actor the request signed with. One per inserted row, none for a kept row, none per batch', NULL)
ON CONFLICT (kind_pattern) DO NOTHING;
