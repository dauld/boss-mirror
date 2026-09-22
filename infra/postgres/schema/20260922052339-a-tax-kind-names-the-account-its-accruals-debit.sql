-- 20260922052339-a-tax-kind-names-the-account-its-accruals-debit.sql —
-- the per-production tax kind names the expense account its accruals
-- debit, so the ledger can read both of its accounts off one row.
--
-- Origin: backlog c0b83e13. The dispatcher rule that accrues a
-- per-production tax spelled the two accounts in its args
-- (liability 2320, expense 6550), and the standalone accrual door
-- POST /api/ledger/tax-accruals took both from the request body with
-- no `kind` — so the posting hold could only check that SOME tax_kinds
-- row named the liability account, never that THIS kind's row did. The
-- rule's args carry the kind now and the ledger resolves both accounts
-- from the row (e021be29 did the liability half).
--
-- 40-ledger.sql seeded this kind with a NULL expense_account, because
-- until now nothing read an expense account off a row: the accrual's
-- expense side was the caller's, held only by a `matches!` allowlist
-- of three demo codes inside rules.rs. An instance that still holds
-- the demo regime gets the account filled in here so its accruals keep
-- posting; the demo tenant's own seeds/tax.toml carries the same value
-- for a fresh publish, and a real instance that evicted the demo rows
-- has no row for this to touch.
--
-- The row is matched by the two facts 40-ledger.sql wrote — liability
-- 2320 with no expense account — rather than by the kind's name, which
-- is the demo tenant's vocabulary and does not belong in a product
-- migration (infra/lint/no-tenant-vocabulary-above-its-tier.sh).
--
-- expense_account IS NULL, so a tenant that has already declared an
-- expense account of its own keeps it: a migration repairs the row the
-- product seeded, it never overwrites a tenant's declaration. 6550 is
-- seeded by 40-ledger.sql, which the FK requires — the EXISTS says so
-- rather than failing the whole run on an instance that evicted it.

UPDATE tax_kinds
   SET expense_account = '6550'
 WHERE liability_account = '2320'
   AND expense_account IS NULL
   AND EXISTS (SELECT 1 FROM gl_accounts WHERE code = '6550');
