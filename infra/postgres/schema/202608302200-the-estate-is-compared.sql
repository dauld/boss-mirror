-- 202608302200-the-estate-is-compared.sql — the compare half of the
-- estate split (59ef456a).
--
-- The observe half (feat/the-estate-is-observed) records what a look
-- at the cluster FOUND as `jobs.estate.observed`; the `nodes` registry
-- says what we MEANT to have. This rule closes the loop: every
-- observation fires the `estate.compare` handler, which reads both
-- sides over HTTP — GET /api/estate/nodes and the observation riding
-- in the event payload — and records the disagreement as ONE
-- `jobs.estate.compared` event via the comparison door
-- (POST /api/estate/comparison).
--
-- EVENTED, NOT ON CADENCE: the packet sketched "a handler on cadence
-- reads the observation", but firing on the observation itself is
-- strictly simpler — no /api/events/tail read, no second clock, and
-- the comparison inherits the observer's own daily cadence. If the
-- observer stops firing, the missing comparison datapoint is the
-- signal (the census's honest-limits posture, unchanged).
--
-- REPORT FIRST, RAISE LATER: no rule listens on jobs.estate.compared —
-- the series is for lenses and for calibrating the eventual raiser,
-- not for the cascade. The loop terminates here by design.
--
-- Scope discipline lives in the handler: the observer's scope is
-- `kubernetes-nodes`, so only declared rows whose role is `talos-*`
-- participate — the forge host and boss-gcp can never be "missing"
-- from an observation that cannot see them.
-- (crates/orchestrators/boss-dispatcher-handlers/src/handlers/estate_compare.rs)

-- Both estate kinds join the registry (108). `jobs.estate.observed`
-- was minted by the observe car without a row here — registering it
-- now keeps the drift guard quiet about a kind we deliberately speak.
INSERT INTO event_kinds (kind_pattern, source, description, suffix_domain) VALUES
  ('jobs.estate.observed', 'jobs', 'One estate observation: what machines a look at the cluster actually found, recorded verbatim by the estate door', NULL),
  ('jobs.estate.compared', 'jobs', 'One estate comparison: declared vs observed for one observation — counts and findings per class (observed-not-declared, declared-not-observed, drift), as a measured series', NULL)
ON CONFLICT (kind_pattern) DO NOTHING;

INSERT INTO dispatcher_rules (name, version, status, on_event, when_expr, do_steps, delay, schedule_cadence, schedule_anchor, schedule_calendar) VALUES
  ('estate-compare-on-observation', 1, 'active', 'jobs.estate.observed', NULL,
   '[{"handler":"estate.compare","args":{}}]'::jsonb,
   NULL, NULL, NULL, NULL)
ON CONFLICT (name, version) DO NOTHING;
