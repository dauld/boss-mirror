-- 20260917170551-a-tenants-chart-of-accounts-leaves-a-fact.sql — the
-- chart-of-accounts batch door records what it inserted.
--
-- Origin: backlog 41af5195 (design 18cf4272, David 2026-09-17). Until
-- this car `gl_accounts` was seeded by 40-ledger.sql with the demo
-- tenant's chart and GET /api/ledger/accounts was the only door: any
-- other tenant could not say what its books are made of without a
-- migration to the product. The chart is TENANT DATA — declared in
-- seeds/chart_of_accounts.toml, published by `boss tenant publish`
-- through POST /api/ledger/accounts/batch, insert-if-absent by code —
-- and the starter chart in 40-ledger.sql stays as the OSS default.
--
-- ONE FACT PER INSERTED ROW, the shape the three tenant batch doors
-- landed on in 20260917071313 (#418): the row as inserted (its minted
-- id, code, name, kind, normal_balance, parent) plus `declared_by`,
-- the actor the request signed with, read from the same stamp `_actor`
-- is read from. Never per kept row — a code the table already held is
-- the SAME account, kept under its registered name (insert-if-absent
-- never renames: the code is what every posting rule and journal line
-- points at), and the batch's answer names the difference — and never
-- per batch. Staged on the transactional outbox inside the insert's
-- own transaction, so the row and its fact commit or roll back
-- together; boss-event-relay moves it to audit_log and the bus.
--
-- Declared here so the kind does not ride inside a passing
-- audit-integrity run unread (infra/lint/emitted-kinds-are-declared.sh).

INSERT INTO event_kinds (kind_pattern, source, description, suffix_domain) VALUES
  ('ledger.account.declared', 'ledger', 'A tenant''s batch inserted one gl_accounts row (POST /api/ledger/accounts/batch, insert-if-absent by code): the row as inserted — id, code, name, kind, normal_balance, parent — plus declared_by, the actor the request signed with. One per inserted row, none for a kept row (a colliding code is the same account, kept under its registered name), none per batch', NULL)
ON CONFLICT (kind_pattern) DO NOTHING;
