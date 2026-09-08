-- 202609031830-event-kinds-credential-rotation.sql — the credential
-- rotation's vocabulary joins the event_kinds registry.
--
-- The maiden rotation (packet 7ee101aa, 2026-09-03) drove mint →
-- install → verify → revoke end-to-end and left ZERO domain events:
-- its provenance existed only as step-completion metadata, findable
-- by archaeology rather than by kind. David's review named it ("it
-- doesn't look like any event data was stored about the minting"),
-- and only review COULD name it — the audit-integrity checker's
-- drift guard sees emitted-but-undeclared, never never-emitted. The
-- five-property protocol demands every state change emit an
-- immutable fact; these four rows declare the facts the rotation
-- path now emits at the moment each becomes true.
--
-- Source is 'jobs', not 'dispatcher': the broker handler owns no
-- database and speaks HTTP (the census-door precedent), so the
-- recording write is boss-jobs' rotation door (`POST
-- /api/credentials/{id}/rotation/{phase}`), whose EventStamp carries
-- the jobs service's source — verified against the stamp
-- construction in boss-jobs credentials/http.rs, the same 'jobs'
-- the census and estate rows above it declare.
--
-- Rows, not a `credential.*` pattern — the 154 reasoning again:
-- these phases are a closed set written in Rust
-- (boss_jobs::credentials::RotationPhase, which a Pg test pins to
-- exactly these rows) with no registry bounding the suffix domain,
-- so a pattern would silence future drift instead of surfacing it.
--
-- THE PAYLOAD RULE, stated here because the descriptions repeat it:
-- identifiers and observed effects only — a token's name or numeric
-- id, a Secret's ns/name/key, a value's LENGTH, the repo a verify
-- read — NEVER a credential value. The handler's module doc owns
-- the rule; these descriptions are its registry echo.
INSERT INTO event_kinds (kind_pattern, source, description, suffix_domain) VALUES
  ('credential.minted',    'jobs', 'The credential broker minted a replacement token at the issuer (payload: credential_id, job_id, token name + numeric id, forge user, scopes, replaced_orphan — identifiers only, never a value)', NULL),
  ('credential.installed', 'jobs', 'A minted credential value was installed into its declared storage location (payload: credential_id, job_id, token name, Secret namespace/name/key, value length in bytes). Recording this phase stamps credentials.rotated_at in the same transaction', NULL),
  ('credential.verified',  'jobs', 'A freshly installed credential was verified by effect — it did the thing it exists for (payload: credential_id, job_id, token name, verify_repo, method)', NULL),
  ('credential.revoked',   'jobs', 'An old token was revoked at the issuer and confirmed absent from its token list (payload: credential_id, job_id, the old token identifier the scoper named, deleted_now, confirmed_dead evidence)', NULL)
ON CONFLICT (kind_pattern) DO NOTHING;
