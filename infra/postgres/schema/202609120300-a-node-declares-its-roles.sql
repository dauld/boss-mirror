-- A node declares its ROLES, as registry data, so what a host runs is
-- derived from what it is (design 9e3e093f, resolved by David
-- 2026-09-11: boss-gcp "isn't part of the kubernetes cluster... I still
-- want it fully managed and maintained by BOSS and protocol"; "Go ahead
-- and retire it quickly").
--
-- WHY. boss-gcp carries sixteen BOSS timers. Six are its own work (the
-- WireGuard bastion, the off-cluster estate observer, the ML batch, its
-- own converge) and ten maintain a SECOND, OLDER BOSS stack on the same
-- host — views-catchup, search-reindex, ledger chores, backup — whose
-- data nothing reads (CLAUDE.md §Doors: "boss-gcp's 127.0.0.1:7900 is a
-- second, older, complete stack with different data"). deploy-services'
-- `units` mode installs every TIMERS row on whatever host runs it,
-- because nothing recorded which rows a host is FOR. This is that
-- record: roles are Classes of `node` Subjects (§9 of CLAUDE.md — a
-- taxonomy is a Class registry row, never an enum), membership is a
-- junction table (the synthetic-class shape 01-registries.sql reserves
-- `member_attribute IS NULL` for), and infra/estate/roles.toml maps each
-- role to the units it needs. The converge reads a host's roles off
-- /api/estate/nodes and installs only those units; the rest it REPORTS
-- and, in a later car, retires.
--
-- `nodes.role` STAYS. It is the primary role the estate page keys on
-- (`bastion` finds the jump host) and the observer compares; this table
-- carries the full set. Collapsing the column into the junction is the
-- next car after the second stack is gone — one fact in two places is
-- a holding action, and this is the one with a test on it
-- (a_node_declares_its_roles_pg).

INSERT INTO classes (subject_kind, code, display_name, member_attribute, metadata, sort_order) VALUES
    ('node', 'wireguard-bastion',     'WireGuard bastion',     NULL, '{"membership": "node_roles", "about": "the hub of the 10.99.0.0/24 overlay; off-VPN operators reach the LAN through it"}'::jsonb, 10),
    ('node', 'off-cluster-observer',  'Off-cluster observer',  NULL, '{"membership": "node_roles", "about": "runs the estate observers from OUTSIDE the cluster they observe, so an outage is seen from a loop that does not need the patient"}'::jsonb, 20),
    ('node', 'ml-batch-host',         'ML batch host',         NULL, '{"membership": "node_roles", "about": "runs the nightly ML inference batch"}'::jsonb, 30),
    ('node', 'legacy-stack',          'Legacy BOSS stack',     NULL, '{"membership": "node_roles", "about": "carries the second, older BOSS stack and the ten chores that maintain its data; a role that exists to be removed"}'::jsonb, 90)
ON CONFLICT (subject_kind, code) DO NOTHING;

CREATE TABLE IF NOT EXISTS node_roles (
    node_id      TEXT NOT NULL REFERENCES nodes(id),
    -- Pinned so the composite FK below can name the Class registry:
    -- a role is a Class of `node`, and only of `node`.
    subject_kind TEXT NOT NULL DEFAULT 'node' CHECK (subject_kind = 'node'),
    role         TEXT NOT NULL,
    declared_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (node_id, role),
    FOREIGN KEY (subject_kind, role) REFERENCES classes(subject_kind, code)
);

-- boss-gcp, as measured on the host 2026-09-12 (ops-requests timer-list
-- and df): the bastion, the off-cluster observer, the ML batch, and the
-- legacy stack. The cluster nodes and the forge declare nothing here
-- yet — their units are converged by other loops (the cluster from
-- infra/cluster/manifests, the forge from infra/forge/install.sh) and a
-- host with no declared roles installs every row exactly as before, so
-- this migration changes no host's behaviour until roles.toml is read.
INSERT INTO node_roles (node_id, role) VALUES
    ('boss-gcp', 'wireguard-bastion'),
    ('boss-gcp', 'off-cluster-observer'),
    ('boss-gcp', 'ml-batch-host'),
    ('boss-gcp', 'legacy-stack')
ON CONFLICT (node_id, role) DO NOTHING;
