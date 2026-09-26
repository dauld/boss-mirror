-- 20260925152020-an-auth-admin-write-is-an-event.sql — the gateway's
-- two auth-administration doors join the event-kinds registry
-- (backlog 17ae5248, split from b3a1772d at its triage 2026-09-25).
--
-- `POST /api/auth/onboard` upserted a local credential and
-- `POST /api/auth/issue-reset` mailed a reset link with no call on the
-- gateway's AuthAudit: a credential created OR OVERWRITTEN through the
-- admin door named nobody, and an admin reset — exactly the act an
-- audit trail exists for — left no trace.
--
--   * `auth.credential.written` — email, outcome (created |
--     overwritten), and the acting session: actor (its username),
--     actor_employee_id and actor_role when the session carries them.
--   * `auth.reset.issued` — email, mailed (whether the link was sent),
--     and the same actor fields.
--
-- Neither payload carries a password or a token. Declared in the same
-- car that first emits them: an emitted-but-undeclared kind is the
-- defect class the audit integrity check exists to catch.
--
-- Same posture as 111 and the break-glass kind: the target email is a
-- claim about a credential file row, not a Subject reference, so no
-- ref-check rules.
INSERT INTO event_kinds (kind_pattern, source, description, suffix_domain) VALUES
  ('auth.credential.written', 'gateway', 'A local credential was written through the auth-admin onboard door (outcome: created | overwritten; names the target email and the acting session; never the password)', NULL),
  ('auth.reset.issued',       'gateway', 'An administrator issued a password reset through the auth-admin door (mailed: true | false; names the target email and the acting session; never the token)', NULL)
ON CONFLICT (kind_pattern) DO NOTHING;
