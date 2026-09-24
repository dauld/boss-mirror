-- 20260924141548-the-marketing-step-plugins-retire.sql — the three
-- marketing step plugins retire with the launch calendar.
--
-- WHY (design 2ea444f5, one car of backlog a8991c86, the example-tenant
-- retirement David decided 2026-09-24). `marketing-brief`,
-- `marketing-launch` and `marketing-attribution` were tiers 1, 4 and 5
-- of `marketing-motion`, a Workflow only the retiring example tenant
-- declares. MEASURED 2026-09-24 through boss-api on the system of
-- record: none of the 63 active Workflow rows has a step of any of the
-- three kinds, and `GET /api/jobs/step-plugins/<kind>/in-flight-count`
-- answered 0 for each (control: sign-off answered 53 on the same
-- connection). The same car deletes their rows from
-- infra/platform/step-plugins/, their JS from infra/step-plugins/, the
-- `marketing-launch` StepType, and the launch-calendar read the
-- `marketing-launch` plugin embedded.
--
-- WHY A MIGRATION. 03-jobs.sql inserted the three rows active, and
-- migrations are append-only history (migrations-append-only.sh), so
-- only a later migration can retire them. The seed publishes the bundle
-- insert-if-missing and never retires a row the bundle stopped
-- declaring, so without this the live rows would stay active, pointing
-- at JS the converge no longer ships — and
-- the_step_plugins_bundle_is_the_migrations_pg.rs, which holds the
-- bundle equal to the rows the migrations leave active, would fail.
-- An UPDATE is not an insert, so migrations-declare-schema-only.sh
-- admits it (202609082130 retired sign-off below v3 the same way).
-- Steps already carrying one of these kinds keep their recorded
-- step_plugin_version; there are none on the system of record.

UPDATE step_plugins
   SET status = 'retired'
 WHERE kind IN ('marketing-brief', 'marketing-launch', 'marketing-attribution')
   AND status = 'active';
