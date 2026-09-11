-- 202609082130-sign-off-plugin-v3.sql — publish sign-off.js v3: the
-- signature follows the decision.
--
-- Origin (David, 2026-09-05 14:2x UTC, feedback 221b4b5c, on the
-- emergency merge's "Approve the train bypass" step): "I tried to
-- approve the emergency merge, but it doesn't appear to have taken."
--
-- MEASURED in the gateway log: 15:40:20 POST …/sign-offs 200 →
-- 15:40:22 PATCH …/metadata 204 (the surface re-saved the unchanged
-- decision with a NEW decided_at) → PUT complete 409
-- {missing_or_stale_roles: [platform-admin]}. A stamp is bound to
-- step_shape_hash(title, metadata) — values included — so the
-- surface's own save made its own signature stale two seconds after
-- taking it. That order is the whole defect; the server's rule is
-- right (a signature is on a specific thing).
--
-- WHAT v3 CHANGES, all in the bundle (infra/step-plugins/sign-off.js;
-- pinned by apps/web/src/steps/signOffPlugin.test.ts against a stub
-- that enforces the server's stamp rule, and by
-- infra/lint/a-signature-follows-its-decision.sh on the source order):
--
--   * One gesture for a single signer: decision PATCH → the user's
--     own signature (passkey ceremony when presence is required) →
--     completion PUT. Never a metadata write after a signature.
--   * A decision a signature already covers is NOT re-saved — the
--     decided_at that stands is the one that was signed. A decision
--     that changes is saved BEFORE the re-signature, and the roster
--     marks the stamps that write made stale, offering them again.
--   * Every stage is shown as it lands — "Decision saved", "Signed as
--     <role>", "Completed", or "Waiting on: <roles>" — and a refusal
--     verbatim (a 409's missing_or_stale_roles re-opens those roles'
--     signature buttons), so a tap never looks like it did nothing.
--
-- v2 was published live through the registry API on 2026-08-19 (no
-- migration; authoring_job_id null), so this file is the first
-- sign-off version that rides a train. Whatever sign-off row is
-- active below v3 retires here, because step_plugins_one_active_per_kind
-- allows one active row per kind: on the system of record that is v2;
-- on a database built from the schema alone (TestDb, a fresh install)
-- it is 135's v1, since v2 never had a migration — a `version = 2`
-- retire was a no-op there and the v3 insert collided with v1 (gate
-- d0c83ae9, 2026-09-08).
-- Steps already open keep step_plugin_version = 2 as their record —
-- the SPA loads the kind's ACTIVE bundle, and both rows name the same
-- file, so what they render is v3 (as it already was: the bundle
-- reaches the cluster from infra/step-plugins/ on every converge,
-- independent of the row). No new step KIND; a registry row plus the
-- bundle, no binary.

UPDATE step_plugins
   SET status = 'retired'
 WHERE kind = 'sign-off' AND status = 'active' AND version < 3;

INSERT INTO step_plugins (
    kind, version, status, label, description, category,
    metadata_schema, frontend_url, owning_team
) VALUES (
    'sign-off', 3, 'active', 'Sign off',
    'Custom Step UX for sign-off steps, v3: the signature follows the decision. One gesture for a single signer — decision saved, then the signer''s own stamp (passkey ceremony when presence is required), then completion — with each stage shown as it lands and any refusal verbatim. Never writes metadata after a signature exists: a decision a signature already covers is not re-saved (2026-09-05: the surface''s own re-save with a fresh decided_at made its own stamp stale and the completion answered 409); a changed decision is saved before the re-signature and the roster names the stamps that made stale. Keeps v2''s case rendering (context_md, else the packet''s briefing or filed message), declared-field contract, the Approve/Reject/Request-changes trio, and per-role signature buttons for multi-party steps.',
    'coordination',
    '{"type":"object","properties":{}}',
    'sign-off.js',
    'platform'
)
ON CONFLICT (kind, version) DO NOTHING;
