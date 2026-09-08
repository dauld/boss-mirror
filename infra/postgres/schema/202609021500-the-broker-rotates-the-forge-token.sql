-- 202609021500-the-broker-rotates-the-forge-token.sql — the
-- credential broker's first rotation rule (packet 7ee101aa, first
-- leg).
--
-- One row, and the row is the credential's registry declaration:
-- which issuer account mints the boss-dev forge write token, which
-- k8s Secret its consumers mount (boss-dev/boss-dev-forge-token),
-- what scopes it carries, and which repo read proves it works. The
-- handler (`credential.rotate.forgejo`,
-- boss-dispatcher-handlers handlers/credential_rotate_forgejo.rs)
-- executes issue → install → verify → revoke as machine steps of the
-- triggering rotate-a-credential packet. Trigger precision comes from
-- the dedicated `credential-rotation` StepType (the gate-verdict
-- precedent) plus a `when` on `subject_id` — a field every step.done
-- payload carries, so the predicate cannot dead-letter on an absent
-- identifier.
--
-- A NEW migration rather than a regenerated 41-dispatcher.sql: 41 is
-- an applied migration, and applied migrations are history
-- (docs/design/schema-migrations.md). rules.toml remains the
-- human-authored source; `dispatcher_rules_seed_matches_toml`
-- compares in BOTH directions, so this row and the rules.toml row
-- must say the same thing. ON CONFLICT DO NOTHING keeps the file
-- re-runnable. Rollback is
-- `UPDATE dispatcher_rules SET status = 'retired'` on the name.
INSERT INTO dispatcher_rules (name, version, status, on_event, when_expr, do_steps, delay, schedule_cadence, schedule_anchor, schedule_calendar) VALUES
  ('broker-rotates-the-boss-dev-forge-token', 1, 'active', 'step.done.credential-rotation',
   'subject_id = "boss-dev-forge-token"',
   '[{"handler":"credential.rotate.forgejo","args":{"forge_user":"\"david\"","secret_namespace":"\"boss-dev\"","secret_name":"\"boss-dev-forge-token\"","secret_key":"\"token\"","scopes":"\"write:repository\"","verify_repo":"\"david/boss\""}}]'::jsonb,
   NULL, NULL, NULL, NULL)
ON CONFLICT (name, version) DO NOTHING;
