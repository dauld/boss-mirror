-- 202609022300-event-kinds-ledger-control-plane.sql — the ledger's
-- control-plane vocabulary joins the event_kinds registry.
--
-- The audit-integrity checker has named
-- `ledger.excise_rate_schedule.upserted` as emitted-but-undeclared on
-- every run since 2026-08-25 (it is the only one of these six already
-- in the live log). The other five are the same hole in the same
-- family, found by diffing boss-ledger's emission sites against the
-- registry while fixing the one the checker named — the
-- 154-event-kinds lesson applied: declaring only the named kind would
-- have re-armed the warning at the first period lock.
--
-- All six are control-plane audit-trail rows ("who changed the
-- registry row and when" — same posture as the excise handler's
-- comment), not projection sources; no rebuilder reads them, so no
-- ref-check rules.
--
-- Rows, not a `ledger.*` pattern — same reasoning as 154: these verbs
-- are a closed set written in Rust with no registry bounding the
-- suffix domain, so a pattern would silence future drift instead of
-- surfacing it.
INSERT INTO event_kinds (kind_pattern, source, description, suffix_domain) VALUES
  ('ledger.excise_rate_schedule.upserted', 'ledger', 'The excise tax curve for a jurisdiction was created or replaced (payload: jurisdiction, effective_from, tiers)', NULL),
  ('ledger.fact.superseded',               'ledger', 'A ledger fact was superseded by a correcting fact', NULL),
  ('ledger.period.created',                'ledger', 'An accounting period was created', NULL),
  ('ledger.period.locked',                 'ledger', 'An accounting period was locked against posting', NULL),
  ('ledger.period.unlocked',               'ledger', 'An accounting period was unlocked — posting reopened', NULL),
  ('ledger.revenue_schedule.created',      'ledger', 'A revenue recognition schedule was created', NULL)
ON CONFLICT (kind_pattern) DO NOTHING;
