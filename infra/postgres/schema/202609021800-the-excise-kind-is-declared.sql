-- ledger.excise_rate_schedule.upserted joins the event_kinds registry.
--
-- WHY. The nightly audit-integrity check has warned "event kinds
-- EMITTED but not declared in the event_kinds registry" for this one
-- kind on every run it makes — the registry is the closure half of
-- the correctness protocol (every fact the log holds is a fact the
-- system declares it can emit), so an emitted-but-undeclared kind is
-- a real, if small, hole in that claim.
--
-- It went unread for days because it rode alongside id-gap ERRORs
-- that made the whole check exit 2 nightly; #116 correctly demoted a
-- gap-with-intact-chain to a warning, the check went green, and this
-- warning kept riding along inside a passing run. Found 2026-09-02 by
-- reading the logs of a check that was passing.
--
-- The emitter is the excise-rate-schedule upsert in the ledger
-- domain; the pattern is exact (no family wildcard) because there is
-- exactly one kind here, and a `.*` pattern would silently bless
-- future siblings nobody has reviewed.
INSERT INTO event_kinds (kind_pattern, source, description, suffix_domain) VALUES
    ('ledger.excise_rate_schedule.upserted', 'ledger', 'An excise rate schedule row was created or updated', NULL)
ON CONFLICT (kind_pattern) DO NOTHING;
