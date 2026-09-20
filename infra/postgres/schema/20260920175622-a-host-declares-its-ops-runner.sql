-- A host declares `ops-runner` — the role that says this machine should
-- be answering ops-request packets (backlog 49ed87b4).
--
-- WHY A ROW AND NOT A READING. The IT world map draws a glyph per
-- machine, and until this role existed a runner could only be named by
-- the requests it HAPPENED to have answered: the map showed the hosts
-- that were working and had no way to draw the host that stopped. An
-- absent glyph is indistinguishable from a runner that does not exist,
-- which is the false-empty class at its most consequential — the map
-- cannot show you the machine that died. The registry is the
-- independent statement of what SHOULD be there, so silence has
-- something to be silence FROM.
--
-- WHICH HOSTS. The two that install a runner from
-- infra/ops/install-ops-runner.sh — the forge (infra/forge/install.sh)
-- and boss-gcp (infra/gcp/install-units.sh) — declare it in
-- infra/estate/estate.toml, which the launcher publishes on every start
-- (insert-if-absent, backlog ee368d0c). No `node_roles` rows here: the
-- tree's declaration is where a machine's roles live now, and a
-- migration that inserted them would be the copy that drifts.
--
-- NOT A roles.toml UNIT ROW. `boss-ops-runner` is deliberately absent
-- from the unit roster (it fires every minute and its product IS
-- packets, so a maintenance-wrap packet pair would drown the board —
-- infra/lint/boss-gcp-converges-itself.sh). This role is vocabulary:
-- who is expected to answer, read by boss_jobs::regions.
INSERT INTO classes (subject_kind, code, display_name, member_attribute, metadata, sort_order) VALUES
    ('node', 'ops-runner', 'Ops-request runner', NULL, '{"membership": "node_roles", "about": "polls the system of record about once a minute for this host''s open ops-request packets and executes the allowlisted verb; the role is what makes a silent runner visible on the map rather than absent from it"}'::jsonb, 50)
ON CONFLICT (subject_kind, code) DO NOTHING;
