-- Declared capacity rounds the way the observer rounds.
--
-- CAUGHT BEFORE THE FIRST COMPARISON, by running the observer's
-- transform against real `kubectl get nodes` output and diffing its
-- numbers against the rows declared hours earlier:
--
--   w-1 disk_gb  declared 928, observed 929
--   w-2 disk_gb  declared 109, observed 110
--
-- Both are the same 1 GiB, converted twice by different rules — the
-- migration floored `ephemeral-storage`, the observer rounds it. A
-- reconcile would have opened a packet on two nodes, every day,
-- forever, for a disagreement about arithmetic rather than about
-- hardware. That is precisely the noisy raiser the census handler warns
-- against: "a noisy raiser trains people to ignore it".
--
-- ONE RULE, STATED ONCE: round to the nearest GiB. The observer does
-- it, so the registry does it. The existing five rows are unaffected —
-- their values round and floor identically, which is why the mismatch
-- only appeared when workers with awkward capacities were added.
--
-- Memory was already consistent (both round), so only disk moves.

UPDATE nodes SET disk_gb = 929 WHERE id = 'w-1' AND disk_gb = 928;
UPDATE nodes SET disk_gb = 110 WHERE id = 'w-2' AND disk_gb = 109;
