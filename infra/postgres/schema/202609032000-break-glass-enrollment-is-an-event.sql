-- 202609032000-break-glass-enrollment-is-an-event.sql — the
-- break-glass hardware-key ceremony joins the event-kinds registry
-- (docs/design/break-glass-is-a-key-you-hold.md, Q1-Q6 resolved on
-- packet e9703d8f).
--
-- Two gateway emissions carry the ceremony:
--
--   * `auth.break-glass.enrolled` — NEW kind, declared here. A
--     hardware credential passed the attestation-gated enrollment.
--     No session is minted by it, so it is not a login event; it is
--     an auth-administration fact (label, credential id, AAGUID —
--     all public material, no PII).
--   * `auth.login.succeeded` / `auth.login.denied` with
--     `method: "break-glass"` — the EXISTING kinds; the method field
--     is how new auth paths join without a schema change, exactly as
--     111-gateway-audit-events.sql intended for the passkey path.
--
-- Declared BEFORE first emission on purpose: an emitted-but-
-- undeclared kind is the defect class the audit integrity check
-- exists to catch (it once rode inside passing runs for days), and
-- the fix for that class is to never create the gap.
--
-- Same posture as the 111 kinds: no subject reference (the enrolled
-- key is deployment infrastructure, not a Subject) and therefore no
-- ref-check rules.
INSERT INTO event_kinds (kind_pattern, source, description, suffix_domain) VALUES
  ('auth.break-glass.enrolled', 'gateway', 'A break-glass hardware credential passed the attestation-gated enrollment ceremony (label: primary | backup; public material only, no PII)', NULL)
ON CONFLICT (kind_pattern) DO NOTHING;
