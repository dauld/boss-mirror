-- 20260926003019-a-calendar-token-rests-as-its-hash.sql — the calendar
-- feed table keeps the SHA-256 of each token, never the token (backlog
-- 4aaff4dc, design 3101c506, decided 2026-09-26).
--
-- WHAT WAS WRONG. The token in /ics/{token}/calendar.ics is the whole
-- authentication of a sessionless feed. The rotate wrote it raw here
-- and into the payload of scheduling.calendar-token.rotated, which the
-- outbox copies to audit_log and the bus; the event tail, export and
-- stream hand that payload to every global-read role, and the guest
-- login mints one (audit-readonly). From this migration on the table,
-- the event and the lookup hold only the digest
-- (crates/core/boss-jobs/src/scheduling/feed_token.rs).
--
-- THE DIGEST IN SQL IS THE DIGEST IN RUST. The UPDATE below applies
-- `feed_token::SQL_DIGEST_OF_TOKEN_COLUMN` to every row, and
-- a_calendar_token_rests_as_its_hash_pg.rs runs THIS FILE against a
-- legacy-shaped table and holds the result equal to
-- `CalendarTokenSha256::of` — and holds this file to that constant's
-- exact text, so the two cannot drift apart.
--
-- `logged_raw` marks a row whose token the log holds in the clear. Every
-- row here was minted before digests, so every row is marked; rebuild
-- sets it from the event it replays (a legacy `token` payload → true, a
-- `token_sha256` payload → false), so a replayed table equals this one.
-- A marked row still OPENS: its token hashes to its digest, and the log
-- cannot be edited. What makes the logged tokens worthless is the bounded
-- revoke run after this lands — POST
-- /api/scheduling/calendar-tokens/logged-raw/revoke, one rotated event
-- per marked row — and never a DELETE here, which the next rebuild would
-- undo by replaying the legacy events.
--
-- ONE DEPLOY, NOT EXPAND/CONTRACT (docs/design/schema-migrations.md).
-- Two deploys exist so an old binary keeps working through a rollout.
-- Here the old binary's shape IS the leak, so it is meant to stop: an old
-- rotate fails the NOT NULL on token_sha256 and rolls back with its
-- event, an old lookup finds no `token` column — each a refusal, and
-- none writes or serves a raw token. Keeping the column for a later
-- contract would keep every raw token at rest until then.
ALTER TABLE tech_calendar_tokens ADD COLUMN token_sha256 TEXT;
ALTER TABLE tech_calendar_tokens ADD COLUMN logged_raw BOOLEAN NOT NULL DEFAULT false;

UPDATE tech_calendar_tokens
   SET token_sha256 = encode(sha256(convert_to(token, 'UTF8')), 'hex'),
       logged_raw   = true;

ALTER TABLE tech_calendar_tokens ALTER COLUMN token_sha256 SET NOT NULL;
ALTER TABLE tech_calendar_tokens
    ADD CONSTRAINT tech_calendar_tokens_token_sha256_key UNIQUE (token_sha256);

-- The UNIQUE above is the lookup index; the old one indexed the token.
DROP INDEX IF EXISTS tech_calendar_tokens_lookup;
ALTER TABLE tech_calendar_tokens DROP COLUMN token;
