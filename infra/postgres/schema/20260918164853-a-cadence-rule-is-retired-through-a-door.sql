-- 20260918164853-a-cadence-rule-is-retired-through-a-door.sql — the
-- cadence registry's two write events are declared.
--
-- Origin: backlog 13d1fff3, left by the department-retro and H4-car-3
-- builders on 2026-09-18. `cadence_rules` had NO write door: no `boss
-- cadence` write verb and no POST route, so a live rule could be
-- re-versioned only by a migration (refused since the cutover stamp
-- in infra/lint/migrations-declare-schema-only.sh) and a retire had
-- no path at all. The department-retro car made `protocol-retro-daily`
-- redundant and the operator had to retire it by name with nothing to
-- run. The door is `POST /api/cadence/rules/{name}/retire` and
-- `POST /api/cadence/rules/{name}/publish` through the CadenceRegistry
-- trait car 3 added, and `boss cadence retire <name>` / `boss cadence
-- publish <file.toml>` in front of it.
--
-- ONE FACT PER ROW WRITTEN, staged on the outbox in the row's own
-- transaction (the stations shape): a publish records the row it
-- inserted, a retire records the row it retired, and nothing records
-- nothing — a retire with no active row is a 404, never a silent 204.
-- The bundle seed publishes through the same adapter, so a fresh
-- deployment's first boot now leaves one `jobs.cadence.published` per
-- rule it lands, as the station seed already does. No migration ever
-- recorded a schedule change before this; `cadence_firings` records
-- what a rule DID, and these two record what a rule IS.
--
-- Declared here so neither kind rides inside a passing
-- audit-integrity run unread (infra/lint/emitted-kinds-are-declared.sh).

INSERT INTO event_kinds (kind_pattern, source, description, suffix_domain) VALUES
  ('jobs.cadence.published', 'jobs', 'A cadence rule version went live: any prior active row of the name retired and the declared version inserted active, in one transaction — the platform bundle seed landing a row, or POST /api/cadence/rules/{name}/publish. Payload is the row written (CadenceRuleSpec) with the actor as _actor', NULL),
  ('jobs.cadence.retired', 'jobs', 'The active cadence rule of a name was retired with no successor (POST /api/cadence/rules/{name}/retire, boss cadence retire): the schedule switched off. Payload is the retired row (CadenceRuleSpec) with the actor as _actor', NULL)
ON CONFLICT (kind_pattern) DO NOTHING;
