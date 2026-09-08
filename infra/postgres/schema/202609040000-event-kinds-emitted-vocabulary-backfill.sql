-- 202609040000-event-kinds-emitted-vocabulary-backfill.sql — the
-- long tail of already-emitted domain events joins the event_kinds
-- registry.
--
-- These 39 kinds are all EMITTED to the outbox today (verified: each
-- reaches `record_event_in_tx` / `record_ledger_event_in_tx` in a
-- non-test path) but none was declared in the event_kinds registry.
-- The runtime drift guard in boss-audit-integrity-check only WARNS,
-- and only once a row of that kind actually lands in the live
-- `audit_log` — so an emitted-but-never-yet-exercised kind stays
-- invisible until someone reads the nightly journal. Two of these
-- holes hit reactively in the last fortnight
-- (`ledger.excise_rate_schedule.upserted` warned for 8 days;
-- `credential.*` needed a same-car declaration).
--
-- This car ships the AUTHORSHIP-TIME catch —
-- `infra/lint/emitted-kinds-are-declared.sh`, a gate lint that fails
-- when a literal-or-const kind emitted in `crates/` matches no
-- event_kinds row. That lint is red on main until these rows land;
-- this migration is what turns it green. The same static diff found
-- every kind below.
--
-- Rows, not family patterns — the 154 / excise / credential
-- reasoning: each domain's event verbs are a closed set written in
-- Rust (the `pub const … : &str = "…"` in each crate's `events.rs`),
-- with no registry bounding the suffix domain. A `<prefix>.*` pattern
-- would silence future drift in that family instead of surfacing it.
--
-- ON CONFLICT DO NOTHING: idempotent, and harmless if a later
-- migration or a merge already declared one of these.
INSERT INTO event_kinds (kind_pattern, source, description, suffix_domain) VALUES
  -- accounts (source 'accounts')
  ('accounts.account.updated',             'accounts', 'An account Subject''s row state after an update (rebuild UPSERTs the projection)', NULL),
  ('accounts.account.deleted',             'accounts', 'An account Subject was deleted (rebuild drops the projection row)', NULL),
  ('accounts.account.team-unassigned',     'accounts', 'Account-team membership was removed', NULL),
  ('accounts.account.note-posted',         'accounts', 'A note was posted against an account', NULL),
  ('accounts.account.note-deleted',        'accounts', 'A note was deleted from an account', NULL),
  ('accounts.support-case.opened',         'accounts', 'A support case opened against an account', NULL),
  ('accounts.support-case.updated',        'accounts', 'A support case''s row state after an update', NULL),
  -- calendar (source 'calendar')
  ('calendar.reservation.cancelled',       'calendar', 'A previously reserved calendar slot was cancelled', NULL),
  -- commerce (source 'commerce')
  ('commerce.service_agreement.upserted',  'commerce', 'A service agreement''s row state (value-primary, rebuild source)', NULL),
  -- content (source 'content')
  ('content.bulletin.updated',             'content', 'A company bulletin''s row state after an update', NULL),
  ('content.bulletin.deleted',             'content', 'A company bulletin was deleted (rebuild drops the projection row)', NULL),
  ('content.bulletin.dismissed',           'content', 'A recipient dismissed a bulletin', NULL),
  ('content.file.attached',                'content', 'A file was attached to a content surface', NULL),
  ('content.file.detached',                'content', 'A file was detached from a content surface', NULL),
  -- inventory (source 'inventory')
  ('inventory.vendor.updated',             'inventory', 'A vendor Subject''s row state after an update', NULL),
  ('inventory.vendor.deleted',             'inventory', 'A vendor Subject was deleted (rebuild drops the projection row)', NULL),
  ('inventory.vendor_contact.upserted',    'inventory', 'A vendor contact''s row state', NULL),
  ('inventory.vendor_contact.deleted',     'inventory', 'A vendor contact was deleted', NULL),
  ('inventory.vendor_contract.upserted',   'inventory', 'A vendor contract''s row state', NULL),
  ('inventory.vendor_interaction.recorded','inventory', 'A vendor interaction was recorded', NULL),
  ('inventory.vendor_interaction.deleted', 'inventory', 'A recorded vendor interaction was deleted', NULL),
  ('inventory.vendor_team.assigned',       'inventory', 'Vendor-team membership was added', NULL),
  ('inventory.vendor_team.unassigned',     'inventory', 'Vendor-team membership was removed', NULL),
  -- kb / catalog (source 'kb')
  ('kb.model.updated',                     'kb', 'An equipment-KB model''s row state after an update', NULL),
  ('kb.model.deleted',                     'kb', 'An equipment-KB model was deleted (rebuild drops the projection row)', NULL),
  -- jobs — step lifecycle (source 'jobs')
  ('jobs.step.stamps_invalidated',         'jobs', 'A step''s prior sign-off stamps were invalidated by a re-open / correcting edit', NULL),
  -- ledger (source 'ledger')
  ('ledger.manual_entry.submitted',        'ledger', 'A manual journal entry was submitted', NULL),
  ('ledger.period.closed',                 'ledger', 'An accounting period was closed', NULL),
  ('ledger.revenue.recognized',            'ledger', 'Revenue was recognized against a revenue schedule', NULL),
  -- people (source 'people')
  ('people.employee.deleted',              'people', 'An employee Subject was deleted (rebuild drops the projection row)', NULL),
  ('people.requisition.opened',            'people', 'A hiring requisition opened (status changes ride the same kind via ON CONFLICT DO UPDATE)', NULL),
  -- scheduling — emitted by boss-jobs (source 'jobs')
  ('scheduling.availability.created',      'jobs', 'A tech availability slot was recorded (PTO, training, manual hold)', NULL),
  ('scheduling.availability.deleted',      'jobs', 'A tech availability slot was deleted', NULL),
  ('scheduling.assignment.created',        'jobs', 'A scheduled assignment was created (tech booked against a target job)', NULL),
  ('scheduling.assignment.deleted',        'jobs', 'A scheduled assignment was deleted', NULL),
  ('scheduling.assignment.status-changed', 'jobs', 'A scheduled assignment''s status changed (tentative → confirmed → completed → cancelled)', NULL),
  ('scheduling.shift-pattern.upserted',    'jobs', 'A tech shift pattern (weekly working-hours template) was upserted', NULL),
  ('scheduling.calendar-token.rotated',    'jobs', 'A tech''s calendar-feed token was rotated', NULL),
  -- shipping (source 'shipping')
  ('shipping.shipment.updated',            'shipping', 'A shipment''s row state after an update', NULL),
  ('shipping.shipment.deleted',            'shipping', 'A shipment was deleted (rebuild drops the projection row)', NULL)
ON CONFLICT (kind_pattern) DO NOTHING;
