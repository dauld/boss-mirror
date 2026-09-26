-- 20260926050506-a-run-keeps-its-work-profile.sql — where a dispatched
-- run's tool time and context went, kept beside its record so the IT
-- department retro can read it every week (backlog 2f23f4c6).
--
-- WHAT WAS MEASURED. On 2026-09-24, 96 builder transcripts (11,399 tool
-- calls, 53.8 run-hours) were read by hand: searching and reading were
-- 45% of the calls, 6.5% of the tool time and 89% of the context bytes;
-- builds and tests were 8% of the calls and 64% of the tool time; 5% of
-- the searches were empty. Nothing recorded it. `boss dispatch --report`
-- already reads each run's transcript for its four token counts, so it
-- now reads the tool calls too and writes the profile here.
--
-- TELEMETRY, NOT A PROJECTION. `agent_runs` is a projection of
-- agents.run.recorded, and its rebuild DELETEs every row and replays the
-- log — a column there that the event does not carry would be wiped by
-- the first rebuild. David, 2026-09-16: "the audit_log just needs to
-- capture all work that is done; sensors record data that don't flow
-- into the audit_log." How a run spent its tool time is a measurement
-- of the work, so this is the surface_opens shape: the row is its own
-- record, no event, no rebuilder, and losing it costs the measurement
-- and nothing else. Keyed on the run id, with no foreign key for the
-- same reason: a rebuild of agent_runs must not cascade into it, and a
-- report records a profile before its run has reached a terminal.
--
-- ONE JSONB, NOT A COLUMN PER NUMBER. The shape is
-- boss_jobs::agent_runs::WorkProfile (tool calls, and calls / wall ms /
-- result bytes for each of search_read, build_test, edit, other; calls
-- before the first edit; searches and empty searches; the top ten files
-- read). Every reader goes through that one type and its rollup, so
-- nothing parses the document twice; a number that needs an index can
-- graduate to a column when a query asks for one.
--
-- RETENTION: none yet. It is one row of about a kilobyte per dispatched
-- run — fewer rows than agent_runs itself, which keeps every run. When
-- the retention registry lands (backlog 16115a17) this table takes a
-- row there like surface_opens.

CREATE TABLE IF NOT EXISTS agent_run_profiles (
    run_id       TEXT PRIMARY KEY,
    profile      JSONB NOT NULL,
    -- When the report read the transcript, on the reporter's clock. A
    -- re-report replaces the row, so this is the latest reading.
    recorded_at  TIMESTAMPTZ NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_agent_run_profiles_recorded
    ON agent_run_profiles (recorded_at);

COMMENT ON TABLE agent_run_profiles IS
  'TELEMETRY: one dispatched run''s work profile — tool calls, wall time and '
  'result bytes by class (search_read, build_test, edit, other), calls before '
  'the first edit, empty searches, most-read files — read from its transcript '
  'by boss dispatch --report. Not an audit event and not a rebuild source; '
  'joined to agent_runs on run_id. Read by the IT department retro through '
  'GET /api/agent-runs/profiles (backlog 2f23f4c6).';
