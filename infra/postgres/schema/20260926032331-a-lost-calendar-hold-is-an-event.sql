-- 20260926032331-a-lost-calendar-hold-is-an-event.sql — `jobs.step.hold_lost`
-- joins the event-kinds registry (backlog 4bdb8150, the round-3 review of
-- car 983696b5).
--
-- A step that holds its assignee's time re-asserts that hold after a
-- racing start (boss-jobs `calendar_hook::reassert_hold`). When a third
-- party reserved the time in the gap, the re-assertion met a conflict and
-- did only `tracing::warn` — the step stayed Active holding nothing, and
-- the one record of that was a log line nobody reads. It is now an event
-- the step write records, and the dispatcher rule
-- `open-a-packet-when-a-step-loses-its-hold` opens the packet a person
-- reads. Declared in the same car that first emits it: an
-- emitted-but-undeclared kind is the defect class the audit integrity
-- check exists to catch.
--
-- THE ROSTER IS DECLARED, so `rule_payload_contract` checks the rule's
-- bindings against it (137's ratchet). Flat, as 137 is: only a binding's
-- root segment is a payload key. `held_by` is an array of reservation
-- rows — the record, copied — and resolves to Absent in the rule DSL, so
-- the scalars beside it are what a rule binds.
INSERT INTO event_kinds (kind_pattern, source, description, suffix_domain, payload_fields) VALUES
  ('jobs.step.hold_lost', 'jobs', 'A step that holds its assignee''s time lost its calendar hold: the re-assertion after a racing start found another reservation holding that time. Names the step, the window and the rows holding it now', NULL,
   '[
      {"name": "job_id",        "type": "uuid",   "note": "the packet the step belongs to"},
      {"name": "step_id",       "type": "uuid",   "note": "the step that holds the time as stored"},
      {"name": "assignee_id",   "type": "string", "note": "whose time: the calendar subject (employee)"},
      {"name": "window_start",  "type": "string", "note": "RFC 3339, the step''s scheduled_at"},
      {"name": "window_end",    "type": "string", "note": "RFC 3339, scheduled_at + duration_minutes"},
      {"name": "held_by_count", "type": "int",    "note": "how many reservations hold the window now"},
      {"name": "held_by",       "type": "array",  "note": "those reservation rows, as the calendar answered"}
    ]'::jsonb)
ON CONFLICT (kind_pattern) DO NOTHING;
