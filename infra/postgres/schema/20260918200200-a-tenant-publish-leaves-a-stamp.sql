-- 20260918200200-a-tenant-publish-leaves-a-stamp.sql — a successful
-- `boss tenant publish` records itself in the database it published
-- into, and the launcher publishes only while there is no such record.
--
-- Origin: backlog 6a8d4972 (design e187198f "the instance is the
-- truth", car 2 of 3, 2026-09-18). MEASURED that day: the services
-- launcher (infra/oss-quickstart/services-launcher.sh ->
-- tenant-launch.sh -> infra/seed-tenant.sh) ran `boss tenant publish`
-- at EVERY container start with no guard, so a converge's ConfigMap
-- rebuild implied a publish, and until car 1 (insert-if-absent by
-- default, `--take` the only overwrite) four doors overwrote a live
-- row on each of them. The decided rule is once per DATABASE: the
-- seeds bootstrap a fresh instance (the OSS quickstart, the
-- playground, a switched database) and after that the instance runs
-- on its data; a new repo row reaches a running instance through an
-- operator's `boss tenant publish` (insert-if-absent) or a boot with
-- BOSS_TENANT_TAKE naming the registries to overwrite.
--
-- THE SHAPE. init.sh's first-start gate is a schema probe
-- (`to_regclass('subject_kinds')`) and the sim's reset baseline is a
-- stamp UPDATE on sim_clock — neither can hold "this tenant was
-- published here, when, by what". So: one row per SUCCESSFUL publish,
-- append-only, never updated or deleted. The FIRST row is the stamp
-- the launcher reads (`boss tenant published` prints its date); a
-- later row is the record of an operator's republish or a
-- BOSS_TENANT_TAKE boot, with what it took. A publish that failed
-- midway leaves no row, so the launcher's DEGRADED retry publishes
-- again — every door is idempotent, which is what makes that safe.
--
-- Written by the verb itself (crates/orchestrators/boss-cli/src/
-- tenant_stamp.rs, `PgStamps`), through BOSS_POSTGRES_URL, the way
-- the sim tenant's baseline stamp is written; a publish run where no
-- database URL is set says so and leaves no row. `boss_commit` is the
-- publishing binary's build commit (`boss --version`'s built-from),
-- not the tenant directory's: the ConfigMap that delivers a tenant
-- directory carries no .git, and a guess is not provenance.

CREATE TABLE IF NOT EXISTS tenant_publishes (
    id            BIGSERIAL PRIMARY KEY,
    tenant_id     TEXT        NOT NULL,
    published_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    published_by  TEXT        NOT NULL,
    boss_commit   TEXT        NOT NULL,
    took          TEXT[]      NOT NULL DEFAULT '{}',
    writes        INTEGER     NOT NULL
);

CREATE INDEX IF NOT EXISTS tenant_publishes_published_at_idx
    ON tenant_publishes (published_at);
