-- 20260926061106-an-archived-message-keeps-its-kind.sql — archiving a
-- message is its own state, `archived_at`, beside `kind` (backlog
-- 9bda9726).
--
-- WHAT WAS WRONG. Every archive door — the inbox's Archive, and the two
-- expire doors the dispatcher calls — wrote `kind = 'archived'`, so the
-- projection forgot whether a message had been a direct or a signal;
-- only its sent event in the log still knew. And the single door had no
-- guard, so archiving an archived message updated it again and recorded
-- a second messages.message.archived. From this migration on the doors,
-- the reads and the rebuild (crates/modules/boss-messages) set and read
-- `archived_at`, and leave `kind` as sent.
--
-- THE BACKFILL IS DERIVED FROM THE LOG, AND LANDS WHERE THE REBUILD
-- LANDS. A row the old doors archived sits here as kind 'archived' with
-- no time. Its kind is the one its LAST sent event carried (a deleted id
-- can be sent again; the rebuild replays the last); its time is the
-- FIRST archived event after that send (the old door could record
-- several; the rebuild now skips a repeat, so the first stands), from
-- the payload's `archived_at` or, for a bare `{id}` payload, the log
-- row's own timestamp. The payload holds the wall clock's nanoseconds
-- and the column holds microseconds: a text cast ROUNDS, where the
-- rebuild's sqlx bind TRUNCATES, so the fraction is cut to six digits
-- before the cast. rebuild_e2e.rs
-- (the_migration_backfills_a_legacy_archived_row_the_way_the_rebuild_does)
-- runs THIS FILE against legacy-shaped rows and holds the result equal
-- to a rebuild of the same log.
--
-- WHAT THE LOG CANNOT ANSWER. A legacy row with no archived event in the
-- log takes its `sent_at` as the time (the earliest it can have been
-- archived); a rebuild would bring it back unarchived, as it would have
-- before this change. A legacy row with no sent event keeps kind
-- 'archived' — there is nothing to recover it from — and it holds the
-- Class retirement below, so a kind still in use is never stranded.
--
-- RE-RUNNABLE. Only a row with kind 'archived' AND no archived_at moves,
-- so a second application changes nothing.
ALTER TABLE messages ADD COLUMN IF NOT EXISTS archived_at TIMESTAMPTZ;

WITH legacy AS (
    SELECT id, sent_at FROM messages
     WHERE kind = 'archived' AND archived_at IS NULL
),
last_sent AS (
    SELECT DISTINCT ON (a.payload->>'id')
           a.payload->>'id' AS id, a.payload->>'kind' AS kind, a.id AS seq
      FROM audit_log a
      JOIN legacy l ON l.id = a.payload->>'id'
     WHERE a.kind = 'messages.message.sent'
     ORDER BY a.payload->>'id', a.id DESC
),
first_archive AS (
    SELECT DISTINCT ON (a.payload->>'id')
           a.payload->>'id' AS id,
           COALESCE(
               regexp_replace(a.payload->>'archived_at', '(\.[0-9]{6})[0-9]+', '\1')::timestamptz,
               a.timestamp
           ) AS archived_at
      FROM audit_log a
      JOIN legacy l ON l.id = a.payload->>'id'
      LEFT JOIN last_sent s ON s.id = a.payload->>'id'
     WHERE a.kind = 'messages.message.archived'
       AND a.id > COALESCE(s.seq, 0)
     ORDER BY a.payload->>'id', a.id
)
UPDATE messages m
   SET archived_at = COALESCE(f.archived_at, l.sent_at),
       kind        = COALESCE(NULLIF(s.kind, 'archived'), m.kind)
  FROM legacy l
  LEFT JOIN last_sent s ON s.id = l.id
  LEFT JOIN first_archive f ON f.id = l.id
 WHERE m.id = l.id;

-- `archived` is no longer a kind a sender may choose: boss-classes lists
-- and validates only rows whose `retired_at` is NULL, so the send door
-- now refuses it. Retired, not deleted — the row stays as the record of
-- what the vocabulary held (precedent: 20260924172233). The NOT EXISTS
-- guard keeps it while any row still carries it.
UPDATE classes c
   SET retired_at = NOW(),
       updated_at = NOW()
 WHERE c.subject_kind = 'message'
   AND c.member_attribute = 'kind'
   AND c.code = 'archived'
   AND c.retired_at IS NULL
   AND NOT EXISTS (SELECT 1 FROM messages m WHERE m.kind = 'archived');
