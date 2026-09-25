-- A stamp dies when the shape it signed leaves the step (backlog
-- c085256d, design 87329a13, option C decided 2026-09-25).
--
-- A sign-off stamp binds the `shape_hash` of the step it attested, and
-- until this car the server honoured any stamp whose hash matched the
-- step's current shape. So content taken off the step and put back
-- (A-B-A) revived a withdrawn approval: approve X (stamp S1), withdraw
-- by Reject or Request changes, PATCH X back, and S1 counted again.
-- From this car on, the edit that moves a step's shape marks every live
-- stamp on it `voided_at` / `voided_by_event` in its own write, and the
-- rebuild applies the same void from the `jobs.step.stamps_invalidated`
-- event the edit records (boss-jobs `rebuild.rs::apply_invalidation`).
--
-- THE ROWS ALREADY WRITTEN ARE RE-JUDGED FROM THE LOG, not from belief.
-- Every shape change of a stamped step already recorded a
-- `jobs.step.stamps_invalidated` event — the PUT in the edit's own
-- transaction, the merge door best-effort just after — carrying no list
-- of stamps, only the step. Every stamp alive on the step at that event
-- was left stale by that edit, so under the new rule that event is the
-- one that killed it. A stamp was alive at an event when it was signed
-- before it: its own `jobs.step.signed_off` event's instant (matched on
-- step, role, signer and shape), or its `stamped_at` where the log no
-- longer holds that event. It is voided by the FIRST such event, dated
-- with that event's instant and naming its id — the derivation the
-- rebuild applies to the same legacy events, so a replay reproduces
-- these values rather than drifting from them.
--
-- Every step, not only open ones, because the rebuild re-judges every
-- step. A completed step is voided here only if a stamp on it was
-- signed before an edit and still sat on the step at completion — which
-- means the content had returned to it: the A-B-A this car closes,
-- recorded honestly. Measured on the live log 2026-09-25 14:50Z: 0
-- `jobs.step.stamps_invalidated` events (7 `jobs.step.signed_off`), so
-- this re-judges no row there; it is for every other instance.
--
-- A void already recorded is kept as it is, and a re-run changes
-- nothing: a stamp is re-judged only while it carries no void.
WITH stamp AS (
    SELECT s.id AS step_id, a.st, a.ord
      FROM steps s
     CROSS JOIN LATERAL jsonb_array_elements(s.sign_offs) WITH ORDINALITY AS a(st, ord)
     WHERE jsonb_typeof(s.sign_offs) = 'array'
       AND EXISTS (
            SELECT 1 FROM audit_log e
             WHERE e.kind = 'jobs.step.stamps_invalidated'
               AND e.payload->>'step_id' = s.id::text)
),
judged AS (
    SELECT st.step_id,
           jsonb_agg(
               CASE WHEN st.st->>'voided_at' IS NOT NULL OR k.event_id IS NULL THEN st.st
                    ELSE st.st || jsonb_build_object(
                             'voided_at', to_jsonb(k.at),
                             'voided_by_event', to_jsonb(k.event_id::text))
               END
               ORDER BY st.ord) AS sign_offs,
           count(k.event_id) FILTER (WHERE st.st->>'voided_at' IS NULL) AS voided
      FROM stamp st
      LEFT JOIN LATERAL (
            SELECT e.event_id, e.timestamp AS at
              FROM audit_log e
             WHERE e.kind = 'jobs.step.stamps_invalidated'
               AND e.payload->>'step_id' = st.step_id::text
               AND e.timestamp > COALESCE(
                     (SELECT min(o.timestamp)
                        FROM audit_log o
                       WHERE o.kind = 'jobs.step.signed_off'
                         AND o.payload->>'step_id' = st.step_id::text
                         AND o.payload->>'role' = st.st->>'role'
                         AND o.payload->>'authority_id' = st.st->>'authority_id'
                         AND o.payload->>'shape_hash' = st.st->>'shape_hash'),
                     (st.st->>'stamped_at')::timestamptz)
             ORDER BY e.id
             LIMIT 1
      ) k ON true
     GROUP BY st.step_id
)
UPDATE steps s
   SET sign_offs = j.sign_offs
  FROM judged j
 WHERE s.id = j.step_id
   AND j.voided > 0;
