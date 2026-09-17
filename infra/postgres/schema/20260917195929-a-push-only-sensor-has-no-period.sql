-- 20260917195929-a-push-only-sensor-has-no-period.sql — a sensor
-- whose source is PUSH-ONLY (boss_jobs::sensors::PUSH_ONLY_SOURCES:
-- `site`, backlog 0b5c5081) is one nothing polls: the service that
-- observed the world records the readings itself through
-- POST /api/sensors/{id}/readings (the gateway's site surface, one
-- `www-visits` reading per page view). Such a row declares no
-- credential (the column's empty string) and no period — and the
-- period column was born with CHECK (every_minutes >= 1), which
-- would have forced the tenant to write a number nothing waits.
-- `0` now spells "never polled"; the registry's own validation still
-- refuses it on a polled source, so a polled sensor's period is
-- unchanged in every row this can hold.
--
-- Idempotent: drop-if-exists then add, the constraint idiom of
-- 20260910151729. The sensors table's CHECK is the auto-named one.

ALTER TABLE sensors DROP CONSTRAINT IF EXISTS sensors_every_minutes_check;
ALTER TABLE sensors ADD CONSTRAINT sensors_every_minutes_check
    CHECK (every_minutes >= 0);

COMMENT ON COLUMN sensors.every_minutes IS
  'Minutes between polls; 0 on a push-only source (nothing polls it — '
  'its readings are recorded by the service that observed them).';
