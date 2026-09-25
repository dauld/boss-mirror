-- 20260925143350-a-tenant-declares-its-departments.sql — the facts the
-- departments registry's write door leaves.
--
-- Origin: backlog 7edf0e97, car 0 of design 8c3e9599's plan (approved
-- by David 2026-09-25). Measured that day: the `departments` registry
-- had no write — GET /api/departments and its readiness read were the
-- whole surface — so the 13 rows 20260919181324-a-department-is-a-
-- subject.sql seeds into every instance could not be changed, and the
-- live instance served the demo tenant's roster to a company whose approved
-- roster is it, product, design, marketing, sales, support, hosting,
-- finance, executive and people.
--
-- POST /api/departments/batch now lands a tenant's declared roster
-- (`boss tenant publish` sends seeds/departments.toml), insert-if-absent
-- by code, with `?mode=take` overwriting a held row — a retirement is a
-- take of `retired = true`. Each inserted row and each row a take
-- changes stages one fact on the outbox in the write's own transaction
-- (the agents door's shape, backlog d9409039), so when a department
-- appeared or was withdrawn, and who did it, is read from the log.
--
-- Declared here so the kinds do not ride inside a passing
-- audit-integrity run unread (infra/lint/emitted-kinds-are-declared.sh).
-- No table changes: the `departments` table already carries every
-- column the declaration writes.

INSERT INTO event_kinds (kind_pattern, source, description, suffix_domain) VALUES
  ('department.declared', 'jobs', 'A tenant''s batch inserted one department row (POST /api/departments/batch, insert-if-absent by code): the declaration as inserted — code, display_name, function, sort_order, retired — plus declared_by, the actor the request signed with. One per inserted row, none for a kept row, none per batch', NULL),
  ('department.updated', 'jobs', 'A take (POST /api/departments/batch?mode=take) changed one held department row: the row as it now reads, the changes (field, from, to) and updated_by. A retirement is one of these, retired false -> true. None for a row already as declared', NULL)
ON CONFLICT (kind_pattern) DO NOTHING;
