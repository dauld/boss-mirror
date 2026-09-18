-- 20260918022108-seed-residue-is-not-a-retirement.sql — a rule row a
-- migration inserted and no file ever authored again is seed residue,
-- and a fresh database stops carrying it.
--
-- Origin: backlog b5f21e82, left by the builder of 70bc5725 (2026-09-18).
--
-- MEASURED. Thirty-one migrations in this directory predate the collapse
-- that made infra/dispatcher/rules/ the dispatcher-rule registry's
-- definition (41ba00cd, 2026-09-11). They are applied history — the
-- checksum guard in migrate.sh refuses an edit — so on every fresh
-- database they still insert sixty-four rule names as active product
-- rows. Thirty-five of those names no product file authors any longer:
-- the demo tenant's thirty-one reactors that moved to
-- examples/brewery/seeds/rules.toml on 2026-09-17 (design e2580840
-- car 4, backlog 105fb702) and four the design-doc corpus indexer's
-- deletion retired (f5da586c: design-decision-flush-queue,
-- design-review-level-sweep, design-review-spawn,
-- maintenance-sweep-doc-status-daily). The boot seed
-- (boss_dispatcher::rules::seed) then retired all thirty-five, on every
-- new instance: churn in every first boot log; retired history a tenant's
-- `boss tenant publish` had to TAKE OVER, landing its row at v(n+1) —
-- above a version its file never named (70bc5725); and from then on a
-- `registry ahead, left alone` line per rule on every publish, forever.
--
-- WHAT THIS DOES. Deletes the rows those migrations inserted under the
-- names below — `source IS NULL` only (a product row: the seed's, a
-- migration's, the SPA editor's), and only where no tenant-sourced row
-- holds the name. On a fresh database this runs after the inserts and
-- before the seed, so the seed retires nothing and a tenant lands at the
-- version its file declares. On a converged database the rows are the
-- retired residue, and they go. On an instance whose tenant ALREADY took
-- a name over (the playground: its row at v2 above the product's retired
-- v1) the NOT EXISTS keeps both rows — the takeover's history stays as
-- it was judged, and that instance's publish keeps reading `registry
-- ahead at v2, left alone` for the file's v1, which is accurate and
-- harmless (docs/tenant-contract.md, "A tenant's own reactors").
--
-- THE EXCEPTION THIS TAKES. dispatcher_rules is an append-only versioned
-- registry (docs/architecture-decisions.md §Dispatcher): a change is a
-- new version, a retirement flips status, nothing is edited or deleted.
-- That posture protects DECISIONS — a version someone published, a rule
-- someone switched off — so a reader can see what was enforced and when.
-- These rows are not a decision anyone made. They were written by a seed
-- mechanism the tree has since replaced, retired by another mechanism
-- minutes later on every instance, and nothing ever fired under them
-- there. Deleting seed residue is not deleting history; it is finishing
-- the collapse the migrations could not finish for themselves. The lint
-- infra/lint/no-migration-writes-a-dispatcher-rule.sh reads a DELETE the
-- same way: it cannot open a second home for a rule, so it is allowed.
--
-- THE LIST IS DERIVED, AND PINNED (CLAUDE.md §9a). It equals: every name
-- an `INSERT INTO dispatcher_rules … VALUES` row in this directory
-- names, minus every name a file under infra/dispatcher/rules/ authors,
-- both read from the tree on 2026-09-18 —
--
--     names inserted by migrations (64) − of those, names a product
--     file authors (29) = 35
--
-- crates/core/boss-dispatcher/tests/seed_residue_migration.rs re-derives
-- it and holds this list equal, in both directions; a product file
-- retired by a later decision is excluded there by name, with its
-- packet, because this file is applied history the moment it lands.
-- One name per line, quoted, nothing else on the line: that is the
-- shape the pin reads.

DELETE FROM dispatcher_rules d
 WHERE d.source IS NULL
   AND NOT EXISTS (
       SELECT 1 FROM dispatcher_rules t
        WHERE t.name = d.name AND t.source IS NOT NULL)
   AND d.name IN (
    'allocate-packaging-on-step-ready',
    'bill-approve-on-bill-approval-step-done',
    'bill-payment-batch-on-bill-payment-batch-step-done',
    'design-decision-flush-queue',
    'design-review-level-sweep',
    'design-review-spawn',
    'excise-accrue-on-production-produce-step-done',
    'forward-billing-done-to-webhook',
    'forward-handoff-done-to-webhook',
    'forward-invoice-paid-to-webhook',
    'forward-invoice-past-due-to-webhook',
    'forward-procurement-done-to-webhook',
    'forward-receiving-done-to-webhook',
    'forward-shipment-done-to-webhook',
    'forward-tax-filing-to-webhook',
    'forward-vendor-invoice-to-webhook',
    'invoice-issue-on-billing-step-done',
    'keg-deposit-settle-on-keg-return-closed',
    'ledger-bill-approve-on-expense-bill-step-done',
    'ledger-bill-payment-batch-on-expense-bill-payment-step-done',
    'maintenance-sweep-doc-status-daily',
    'parts-consume-on-production-consume-step-done',
    'parts-consume-on-repair-step-done',
    'payroll-submit-on-payroll-release-step-done',
    'people-hire-on-hr-hire-step-done',
    'people-terminate-on-hr-terminate-step-done',
    'po-place-on-procurement-step-done',
    'products-consume-on-invoice-created',
    'products-produce-on-production-produce-step-done',
    'receive-on-receiving-step-done',
    'shipment-side-effects-on-shipment-step-done',
    'spawn-keg-return-on-delivery',
    'spawn-restock-on-low-inventory',
    'spawn-tasting-panel-on-brew-close',
    'tax-remit-on-tax-remittance-step-done'
   );
