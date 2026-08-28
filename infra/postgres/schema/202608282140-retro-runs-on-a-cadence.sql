-- protocol-retro runs on a cadence, as a ROW.
--
-- This is the end of a thread that started with a stale binary. David,
-- 2026-08-28: "Do we just have a new job protocol on a timer?" — and
-- then, when told it needed a systemd unit: "Why isn't protocol-retro
-- just a data change though?" It should have been, and now is.
--
-- WHAT HAD TO MOVE FIRST, none of it visible from the question:
--   * The dispatcher could only ever run `boss train <verb>`, so no
--     protocol but the conductor could be scheduled (car a91c2d94).
--   * `weekly` was inexpressible: clock fires daily, and wall
--     re-anchors at midnight so 10080 minutes floors to zero.
--   * Recurrence was defined three times in the tree, so adding a
--     fourth here would have been the wrong fix (car cd09048f moved it
--     to boss_core::calendar, beside the BusinessCalendar it already
--     used).
--   * This table's verb CHECK still refused `open:<kind>`, which made
--     the dispatcher's new capability unstorable.
--
-- DAILY, FOR NOW, AND THAT IS A ROW NOT A REWRITE. David, 2026-08-28:
-- "I actually want protocol-retro daily anyways for now. We are learning
-- too fast." The earlier draft of this file argued at length for weekly
-- — that a daily report is noise, and noise is how a report stops being
-- read. That argument holds at a steady state and does not hold now:
-- one session on 2026-08-28 produced fourteen classified errors, six
-- protocol versions and nine filed items. At that rate a week-long
-- window buries its own findings.
--
-- The point worth keeping is that this is a ONE-WORD change to a data
-- value, not a code change. `Basis::Calendar` takes any
-- boss_core::calendar::Cadence, so moving between daily, weekly and
-- monthly is editing this row — which is exactly what "why isn't it
-- just a data change" was asking for. Change it back when the learning
-- rate settles.
--
-- ANCHORED ON A FRIDAY (2026-08-28), which is the window the first
-- retro covers, so every subsequent firing lands a clean week after the
-- one before it.
--
-- 06:10Z, DELIBERATELY OFF THE GRID. train-reconcile fires every ten
-- minutes anchored at midnight, and 134-cadence-window-off-grid.sql
-- records what sharing a tick with it costs: the conductor's flock
-- admits one, and the twice-daily window lost every time — it never
-- once boarded a train. Opening a packet takes no conductor lock, so
-- that exact contention does not apply here, but 06:05 is already
-- train-window's and stacking a third rule on a boarding minute asks a
-- future reader to untangle it. :10 is clear of both.
--
-- THE SINGLE-OPEN CONTRACT does the rest: if last week's retro is still
-- open, the rule leaves it alone rather than filing a second. An
-- unfinished retro stays one visible packet, not a pile.
--
-- NO BUSINESS CALENDAR, deliberately NULL. The cadence loop refuses a
-- rule that names one, because resolving a code to its closed days
-- lives in the calendar service and this loop holds no dependency on
-- it — a scheduler that stops scheduling when another service is down
-- would be worse than the duplication all of this replaced. A retro
-- landing on a public holiday is a packet waiting a day, which is not
-- a problem worth that trade.

INSERT INTO cadence_rules
    (name, version, status, verb, basis,
     every_minutes, at_times, min_dock_depth, cooldown_minutes,
     cadence, anchor_date, business_calendar)
VALUES
    ('protocol-retro-daily',  1, 'active', 'open:protocol-retro', 'calendar',
     NULL, '["06:10"]'::jsonb, NULL, NULL,
     'daily',  DATE '2026-08-28', NULL)
ON CONFLICT (name, version) DO NOTHING;
