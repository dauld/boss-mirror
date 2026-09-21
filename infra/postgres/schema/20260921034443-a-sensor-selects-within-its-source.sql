-- A sensor may select ONE STREAM within its source.
--
-- WHY (design bffc0aba, David 2026-09-21: "use inbox names to trigger
-- different protocols"). Mail is the first source where several sensors
-- read the SAME place and each must take only its own: support@ and
-- finance@ both route into one Proton mailbox, and each opens a
-- different department's protocol. A sensor row already says WHAT to
-- open; without a selector it cannot say WHICH stream it opens it FROM,
-- so the routing could not be registry data at all and the fallback
-- would be a branch per department inside one shared protocol — a match
-- statement living in a workflow row, which is what CLAUDE.md §9 exists
-- to prevent.
--
-- NULLABLE, and null is the truth for every existing row: Stripe's
-- restricted key IS the selection, and the site sensor is push-only.
-- No backfill, because there is nothing to backfill — a source with one
-- stream selects nothing, and that is a different fact from selecting
-- an empty string, which the declaration refuses by name.
ALTER TABLE sensors ADD COLUMN IF NOT EXISTS selector TEXT;

-- Empty is refused at the door (validate_sensor) and again here, so a
-- row that reached the table another way cannot mean "everything" by
-- accident: a selector that silently matched all mail would hand one
-- department every other department's inbox.
ALTER TABLE sensors DROP CONSTRAINT IF EXISTS sensors_selector_not_blank;
ALTER TABLE sensors ADD CONSTRAINT sensors_selector_not_blank
    CHECK (selector IS NULL OR length(btrim(selector)) > 0);
