-- 202609082000-a-landed-car-advances-its-backlog-item.sql — the
-- car-merge obligation learns to ROUTE an untriaged backlog item, as
-- rule data (dda0713c).
--
-- WHY. `boss gate --park-backlog-item <id>` stamps the item on the
-- car's `backlog_item` edge. The car boards, merges, converges,
-- arrives, is proven, and closes `merged` — and the item stayed at
-- `triage: ready`, untouched (f3796323 on 2026-09-08; five such items
-- closed by hand that night). The rule already fired at the right
-- moment: ship-a-change's `merged` terminal is
-- `ready_when = "steps.proven.done"`, so `jobs.job.closed` with
-- `outcome = "merged"` IS the car proven in production. What v2 could
-- not do was act: `build` was `pending`, `triage` was the open step,
-- and the handler wrote an `obligation_noop` on the car instead —
-- "triage is a routing decision an obligation must not make".
--
-- For a backlog item that reasoning inverts. Triage asks an operator
-- to measure a claim and choose a route; a car that landed and was
-- proven is the measurement, and `build` is the route. So v3 carries
-- a `route`: the KIND whose triage vocabulary it speaks (scoped —
-- `user-feedback` keeps the v2 answer, a filer's decision is still
-- theirs), the routing STEP, and the METADATA that step requires at
-- done (`disposition` + `evidence`, per the live Workflow; pinned by
-- boss-dispatcher's feedback_obligation_rules.rs). The handler
-- completes triage, re-reads, and the fork's own `ready_when` has
-- opened `build`, which `steps` then completes with the proof; the
-- item's `closed` outcome does the rest on the dispatcher's tick —
-- the same two writes the hand close made, through the same door.
--
-- THE TRANSLATION IS DATA: the handler knows "route through this
-- step with this vocabulary if it is open"; which kind, which step,
-- which disposition are this row's statement, editable by publishing
-- v4, never by a deploy. Idempotent: a redelivery finds the item
-- closed (guard 1) or `build` completed (guard 2) and writes nothing.
--
-- RETIRE v2 BEFORE INSERTING v3 (the 148 lesson):
-- `dispatcher_rules_one_active_per_name` rejects two active versions,
-- and this file runs in one transaction so there is no window where
-- the obligation is missing.
UPDATE dispatcher_rules
   SET status = 'retired'
 WHERE name = 'complete-feedback-branch-on-car-merged'
   AND version = 2;

INSERT INTO dispatcher_rules
    (name, version, status, on_event, when_expr, do_steps,
     delay, schedule_cadence, schedule_anchor, schedule_calendar)
VALUES
  ('complete-feedback-branch-on-car-merged', 3, 'active', 'jobs.job.closed',
   'kind = "ship-a-change" AND outcome = "merged"',
   '[{"handler":"jobs.complete_linked_step","args":{"link":"\"backlog_item\"","steps":"\"investigate,design-review,build\"","done_metadata":"\"{\\\"verdict\\\": \\\"approved\\\", \\\"answer\\\": \\\"shipped: {branch} — {title}\\\"}\"","route":"\"{\\\"kind\\\": \\\"backlog-item\\\", \\\"step\\\": \\\"triage\\\", \\\"metadata\\\": {\\\"disposition\\\": \\\"build\\\", \\\"evidence\\\": \\\"shipped and proven: {branch} — {title} (car {car})\\\"}}\""}}]'::jsonb,
   NULL, NULL, NULL, NULL)
ON CONFLICT (name, version) DO NOTHING;
