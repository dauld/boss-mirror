-- The gate's park intent is a declared Job reference (backlog 89faab68).
--
-- FOUND BY THE COUNT, NOT BY REVIEW. The first production run of the
-- census's undeclared-reference scan (design c0d2787a, q4) reported six
-- Job references sitting in fields nobody had declared. Two were not
-- hand-authored prose: `park_backlog_item`, which `boss gate
-- --park-backlog-item` writes onto EVERY gate-run packet it stamps a
-- park intent on. The pipeline itself had been carrying an undeclared
-- Job reference on every car it gated.
--
-- WHAT THE DECLARATION BUYS, none of which is tidiness. An undeclared
-- field is not ref-checked, so a park intent naming a packet that does
-- not resolve is admitted in silence — and the park intent is exactly
-- what the auto-park rule reads to attach a car to its item, so one
-- that resolves to nothing attaches a car to nothing. It does not
-- render in the Links panel, so reading a gate-run does not show which
-- item the car answers. And it cannot be queried: "which gate-runs
-- park against this item" has no answer short of scanning metadata.
--
-- THE BEHAVIOUR CHANGE, stated rather than discovered. `on_missing`
-- takes the table default, `abort` since 202608291630, so from here a
-- park intent naming a packet that does not exist is REFUSED at the
-- write. Measured before declaring it: of the last 40 gate-runs, 28
-- carry `park_backlog_item` and all 28 hold a full 36-character id,
-- zero prefixes — because the verb resolves the flag before stamping
-- it and prints the resolution. So nothing being written today is
-- refused by this.
--
-- 'gate-run', NOT '*'. The three relation edges are wildcards because a
-- relationship is not a property of a kind. This is the opposite: it is
-- written by one verb onto one kind, so it is declared on that kind, the
-- way `ship-a-change.backlog_item` is. A wildcard would invite the field
-- onto packets that have no business carrying it.
INSERT INTO job_edges (source_kind, field_path, field_kind, description) VALUES
  ('gate-run', 'park_backlog_item', 'job_id',
   'The backlog/feedback Job the car this gate parks will answer')
ON CONFLICT (source_kind, field_path) DO NOTHING;
