-- A cadence rule can fire on a CALENDAR, and can name a packet to open.
--
-- TWO GAPS, ONE OF THEM SELF-INFLICTED.
--
-- 1. WEEKLY WAS NOT EXPRESSIBLE. `clock` fires at a time of day EVERY
--    day and has no day filter; `wall` re-anchors at midnight UTC, so
--    every_minutes = 10080 (a week) floors to zero and fires daily too.
--    Putting protocol-retro on a weekly schedule — the thing the whole
--    exercise was for — could not be written as a row.
--
-- 2. THE VERB CHECK STILL REFUSED `open:<kind>`. The cadence dispatcher
--    learned to open a packet of any workflow kind (car a91c2d94,
--    merged on train #137), but this table's CHECK still only allowed
--    the four conductor verbs — so the Rust would parse a row the
--    database would never accept. A half-feature, and mine: the car
--    shipped the parser and the dispatch without the column that has to
--    hold the value. Found by reading the constraint before writing a
--    row against it rather than after.
--
-- WHY A CALENDAR BASIS AND NOT A `days` COLUMN. `boss_core::calendar`
-- now owns Cadence and its firing math (car cd09048f, design a02b01e0),
-- beside BusinessCalendar which that math already consumed. Reusing it
-- means "weekly" has ONE definition in the tree instead of the three it
-- had this morning — and it brings Monthly/Quarterly/Annually and the
-- business-day postponement along for free, rather than this table
-- growing a private notion of recurrence that drifts from the
-- dispatcher's.
--
-- BUSINESS CALENDAR IS PER-SCHEDULE, NOT GLOBAL (design a02b01e0 Q3,
-- David): deferring maintenance to the next working day is right;
-- deferring a reconcile that runs every ten minutes is not. So it is a
-- nullable column on the rule, and absent means every day is a business
-- day.
--
-- A CALENDAR RULE STILL NEEDS A TIME OF DAY. "Weekly" says which days;
-- `at_times` says when on those days. The column is reused rather than
-- duplicated, and the check below requires exactly one time — a weekly
-- rule with two times is almost certainly a clock rule that was
-- mislabelled.

ALTER TABLE cadence_rules
    ADD COLUMN IF NOT EXISTS cadence           TEXT,   -- calendar: daily|weekly|biweekly|monthly|quarterly|annually|hourly|every-<n>-minutes
    ADD COLUMN IF NOT EXISTS anchor_date       DATE,   -- calendar: the date the recurrence is anchored to
    ADD COLUMN IF NOT EXISTS business_calendar TEXT;   -- calendar: optional; absent = every day is a business day

-- The verb may now be a conductor verb OR `open:<kind>`. The kind is
-- pinned to kebab-case here for the same reason the Rust pins it: the
-- value lands in a URL query and a JSON body, and a kind that needs
-- escaping is a kind that is wrong.
ALTER TABLE cadence_rules DROP CONSTRAINT IF EXISTS cadence_rules_verb_check;
ALTER TABLE cadence_rules ADD CONSTRAINT cadence_rules_verb_check CHECK (
    verb IN ('preflight', 'reconcile', 'board', 'run')
    OR verb ~ '^open:[a-z0-9-]+$'
);

ALTER TABLE cadence_rules DROP CONSTRAINT IF EXISTS cadence_rules_basis_check;
ALTER TABLE cadence_rules ADD CONSTRAINT cadence_rules_basis_check
    CHECK (basis IN ('wall', 'clock', 'queue-depth', 'calendar'));

-- The per-basis parameter check, restated whole. Postgres has no way to
-- extend an anonymous CHECK, and restating it is better than adding a
-- second constraint that has to be read alongside the first to know
-- what is legal.
ALTER TABLE cadence_rules DROP CONSTRAINT IF EXISTS cadence_rules_check;
ALTER TABLE cadence_rules DROP CONSTRAINT IF EXISTS cadence_rules_params_check;
ALTER TABLE cadence_rules ADD CONSTRAINT cadence_rules_params_check CHECK (
    (basis = 'wall'
        AND every_minutes IS NOT NULL AND every_minutes > 0
        AND at_times IS NULL AND min_dock_depth IS NULL
        AND cooldown_minutes IS NULL
        AND cadence IS NULL AND anchor_date IS NULL AND business_calendar IS NULL)
 OR (basis = 'clock'
        AND at_times IS NOT NULL
        AND every_minutes IS NULL AND min_dock_depth IS NULL
        AND cooldown_minutes IS NULL
        AND cadence IS NULL AND anchor_date IS NULL AND business_calendar IS NULL)
 OR (basis = 'queue-depth'
        AND min_dock_depth IS NOT NULL AND min_dock_depth > 0
        AND cooldown_minutes IS NOT NULL AND cooldown_minutes > 0
        AND every_minutes IS NULL AND at_times IS NULL
        AND cadence IS NULL AND anchor_date IS NULL AND business_calendar IS NULL)
 OR (basis = 'calendar'
        AND cadence IS NOT NULL
        AND anchor_date IS NOT NULL
        -- Exactly one time-of-day: the cadence chooses the DAYS, this
        -- chooses WHEN on them.
        AND at_times IS NOT NULL AND jsonb_array_length(at_times) = 1
        AND every_minutes IS NULL AND min_dock_depth IS NULL
        AND cooldown_minutes IS NULL)
);
