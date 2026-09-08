-- 202609021400-a-keg-deposit-is-a-liability.sql — the full
-- balance-sheet keg model (93f936b9; David's Q1 decision, design
-- review 2026-08-22: "Let's go for the full balance-sheet model").
--
-- Kegs are returnable containers: the customer's deposit is money the
-- brewery HOLDS, not money it earned. Until now the deposit existed
-- only as flow data on the keg-return protocol (v4 live: log-fleet-out
-- records kegs_out + deposit_cents; receive-returns records
-- kegs_returned + kegs_lost) and never touched the books — the
-- commercial audit's gap #1. This migration gives the ledger the two
-- accounts and the rebuild bridge; the posting rules live in
-- boss-ledger/src/rules.rs (finance.keg_deposit.charged / .released),
-- and the dispatcher rule below fires the settlement handler when a
-- keg-return packet reconciles.
--
-- Double-entry design:
--   fleet out     DR 1000 Cash / CR 2400 Keg Deposits Payable
--   fleet back    DR 2400 (full deposit)
--                 CR 1000 refund      (deposit × returned / out)
--                 CR 4150 forfeiture  (the lost-keg remainder)
-- The release REQUIRES kegs_returned + kegs_lost == kegs_out, so 2400's
-- standing balance is exactly the deposits of fleets still in the
-- field — never a liability owed to nobody.

INSERT INTO gl_accounts (id, code, name, kind, normal_side) VALUES
    -- Credit-normal: grows as fleets ship, drains as they reconcile.
    ('00000000-0000-0000-0000-000000002400', '2400', 'Keg Deposits Payable', 'liability', 'credit'),
    -- Breakage income: the lost-keg share of a released deposit. Kept
    -- out of 4100-4140 (beer revenue) so sales mix stays honest.
    ('00000000-0000-0000-0000-000000004150', '4150', 'Keg Deposit Forfeitures', 'revenue', 'credit')
ON CONFLICT (code) DO NOTHING;

-- Rebuild bridge: the settlement endpoint
-- (POST /api/ledger/keg-deposit-settlements) emits one audit event per
-- fact, payload verbatim, so a TRUNCATE-then-replay rebuild reproduces
-- both facts from audit_log alone. `keg_deposit_settlements` is a
-- provenance label only — there is no such table; source_table is
-- written verbatim and never joined (same contract as 'tax_accruals').
INSERT INTO gl_fact_projection_rules (event_kind, fact_kind, source_table, source_id_path, happened_on_path, created_by_path) VALUES
    ('ledger.keg_deposit.charged',  'finance.keg_deposit.charged',  'keg_deposit_settlements', '/charge_id',  '/shipped_on',  NULL),
    ('ledger.keg_deposit.released', 'finance.keg_deposit.released', 'keg_deposit_settlements', '/release_id', '/returned_on', NULL)
ON CONFLICT (event_kind) DO NOTHING;

-- Both kinds join the event-kind registry so the drift guard stays
-- quiet about kinds we deliberately speak.
INSERT INTO event_kinds (kind_pattern, source, description, suffix_domain) VALUES
    ('ledger.keg_deposit.charged',  'ledger', 'A keg fleet shipped with a refundable deposit (DR 1000 Cash / CR 2400 Keg Deposits Payable)', NULL),
    ('ledger.keg_deposit.released', 'ledger', 'A keg fleet reconciled: deposit refunded for returned kegs, forfeited for lost (DR 2400 / CR 1000 + CR 4150)', NULL)
ON CONFLICT (kind_pattern) DO NOTHING;

-- The wiring: a reconciled keg-return packet settles its deposit. The
-- protocol's field-bearing steps are plain `task` kind, so the honest
-- trigger is the packet's terminal close (`step.done.task` would wake
-- on every task step in every workflow — the same reasoning as
-- spawn-car-on-sweep-remediated). The handler reads both legs' counts
-- and completion dates from the closed job's steps and posts them
-- with their own happened_on dates, so the ledger timeline carries the
-- in-field window even though both legs post at reconciliation.
-- Present in infra/dispatcher/rules.toml because
-- `dispatcher_rules_seed_matches_toml` compares in BOTH directions.
INSERT INTO dispatcher_rules (name, version, status, on_event, when_expr, do_steps, delay, schedule_cadence, schedule_anchor, schedule_calendar) VALUES
    ('keg-deposit-settle-on-keg-return-closed', 1, 'active', 'jobs.job.closed',
     'kind = "keg-return" AND outcome = "completed"',
     '[{"handler":"ledger.keg_deposit.settle","args":{}}]'::jsonb,
     NULL, NULL, NULL, NULL)
ON CONFLICT (name, version) DO NOTHING;
