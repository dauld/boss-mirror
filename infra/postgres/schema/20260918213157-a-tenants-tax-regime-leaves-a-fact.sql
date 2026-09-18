-- 20260918213157-a-tenants-tax-regime-leaves-a-fact.sql — the tax
-- batch door records what it inserted.
--
-- Origin: backlog 7f163e58 (design e187198f, David 2026-09-18: the
-- instance is the truth and the tenant contract declares its chart).
-- Until this car `tax_kinds` and `sales_tax_rate_by_state` were seeded
-- by 40-ledger.sql with the demo tenant's regime and nothing else
-- could declare a filing kind — the accrual door refuses an
-- unregistered one with "register it in tax_kinds first", and the only
-- way to register one was a migration to the product. Each kind's FK
-- also pinned a GL account (2150 / 2300 / 2310 / 2320 / 6500) on every
-- real instance, because the eviction that clears the demo chart had
-- to keep an account a tax kind names. The regime is TENANT DATA —
-- declared in seeds/tax.toml, published by `boss tenant publish`
-- through POST /api/ledger/tax/batch, insert-if-absent by kind and by
-- state — and the demo tenant carries the migration's rows in its own
-- seed, so they leave a real instance with the rest of the demo's
-- reference rows (infra/postgres/example-reference-rows.sh).
--
-- The rows 40-ledger.sql inserted are NOT deleted here: an applied
-- migration is history, and a row leaves an instance through the
-- bounded verb with the record on its packet, or at a fresh instance's
-- first boot — never through a migration. Schema only.
--
-- ONE FACT PER INSERTED ROW, the shape the tenant batch doors landed on
-- in 20260917071313 (#418) and the chart door in 20260917170551: the
-- row as inserted plus `declared_by`, the actor the request signed
-- with. Never per kept row (a kind or state the table already held is
-- kept as registered and the batch's answer names the difference),
-- never per batch. Staged on the transactional outbox inside the
-- insert's own transaction.
--
-- Declared here so the kinds do not ride inside a passing
-- audit-integrity run unread (infra/lint/emitted-kinds-are-declared.sh).

INSERT INTO event_kinds (kind_pattern, source, description, suffix_domain) VALUES
  ('ledger.tax_kind.declared', 'ledger', 'A tenant''s batch inserted one tax_kinds row (POST /api/ledger/tax/batch, insert-if-absent by kind): the row as inserted — kind, liability_account, expense_account, derive_basis — plus declared_by, the actor the request signed with. One per inserted row, none for a kept row, none per batch', NULL),
  ('ledger.sales_tax_rate.declared', 'ledger', 'A tenant''s batch inserted one sales_tax_rate_by_state row (POST /api/ledger/tax/batch, insert-if-absent by state): the row as inserted — state, jurisdiction, rate_bps — plus declared_by, the actor the request signed with. One per inserted row, none for a kept row, none per batch', NULL)
ON CONFLICT (kind_pattern) DO NOTHING;
