-- The forge declares `cluster-operator` — the role that makes it the host
-- cluster management runs on, so the workstation is a terminal (design
-- 1bc4b4ed; Q1 resolved "forge now" by David, 2026-09-12).
--
-- What the role brings is installed by infra/forge/install.sh when
-- forge-converge reads this row off /api/estate/nodes: talosctl pinned
-- by sha, beside the kubectl the host already carries; and a check —
-- never a write — that /etc/boss-ops/talosconfig and
-- /etc/boss-ops/kubeconfig are present, root-owned and mode 0600.
-- Credentials are placed by David once (token admin is his); the
-- converge reports their absence on its packet until they are.
--
-- Why the forge, said once here so the row explains itself: it is
-- already a managed host — converges itself from main, runs the
-- ops-runner, reports its units, exposes its journal — so the role is
-- a data row plus an install step, where a dedicated ops host (w-2
-- reimaged, a GCP VM) is a project. Both stay recorded as the answer
-- if concentrating the cluster's root credential beside git and the
-- registry ever needs acting on.
INSERT INTO classes (subject_kind, code, display_name, member_attribute, metadata, sort_order) VALUES
    ('node', 'cluster-operator', 'Cluster operator', NULL, '{"membership": "node_roles", "about": "holds talosctl, kubectl and the cluster credentials under /etc/boss-ops; where cluster management commands run, so a workstation is only a terminal"}'::jsonb, 40)
ON CONFLICT (subject_kind, code) DO NOTHING;

INSERT INTO node_roles (node_id, role) VALUES
    ('forge', 'cluster-operator')
ON CONFLICT (node_id, role) DO NOTHING;
