# Platform step-plugin bundle — the ROW as data, beside the JS

One file per plugin: `<kind>.toml` holds exactly one `[[step_plugin]]`
whose `kind` is the file name, carrying every column of the
`step_plugins` row (`03-jobs.sql`): `kind`, `version`, `status`,
`label`, `description`, `category`, `metadata_schema` (the JSON Schema
as a TOML table), `frontend_url`, `owning_team`. **Adding a plugin is
dropping a file in here and its JS in `infra/step-plugins/`.**
Changing a row is bumping its `version`.

A step plugin is two artefacts. The JS bundle has always been data in
the tree (`infra/step-plugins/<name>.js`, mounted into the cluster as
the `step-plugins` ConfigMap the converge runner rebuilds on every
deploy). The ROW that names it was not: until 2026-09-18 (backlog
393d3234, consolidation H4, car 2) seven migrations were its only home
— `03-jobs.sql` seeded seven kinds, `126` / `135` / `146` / `155` /
`156` one each, and `202609082130` retired `sign-off` below v3 and
inserted v3. Those files stay exactly as they are (migrations are
append-only history); what changed is that
`infra/lint/migrations-declare-schema-only.sh` refuses `INSERT INTO
step_plugins` in any migration newer than its cutover stamp, and
`the_step_plugins_bundle_is_the_migrations_pg.rs` holds this directory
equal, column for column, to the active rows those migrations produce
— and proves the seed can rebuild an emptied table from this directory
alone.

**Why here and not beside the JS.** Measured against what already
reads each directory: the quickstart image copies `infra/platform`
whole and nothing copies `infra/step-plugins` into it — the seed runs
inside the image, the JS is mounted from the tree. A row beside its JS
would have needed a Dockerfile COPY, a seed flag and an init.sh edit
to be found; a row here is found the way the station bundle is, as
the sibling of `--seed-path`, and no launcher changed.
`infra/lint/step-plugin-bundle-exists.sh` and the
`platform_step_plugins_bundle` test hold each row to a JS file that
exists.

**Published by `boss-platform-workflow-seed`**, the same binary and
the same `--seed-path` every launcher already passes, after the
workflows and the stations (`--step-plugins-path` overrides).
Insert-if-missing by (kind, version), through
`boss_jobs::step_plugin_seed::seed_step_plugins` — one decision table
for every bundle, `boss_jobs::bundle_seed`:

| live registry                                   | outcome                 |
|-------------------------------------------------|-------------------------|
| no row of this kind                             | insert, active          |
| active row at a LOWER version                   | publish; retire the old |
| row at (kind, version) active and EQUAL         | present — untouched     |
| row at (kind, version) active and DIFFERENT     | REFUSED, by field       |
| row at (kind, version) exists but is not active | superseded — untouched  |
| no row at (kind, version); active row is HIGHER | behind — untouched      |

The refusal is the point: a file edited without a version bump would
otherwise disagree silently with every deployment that already has
the row. Live authoring (`POST /api/jobs/step-plugins`, then
`.../{kind}/publish`) is untouched — `sign-off` v2 arrived that way on
2026-08-19 — and an operator's later version supersedes the file's;
the seed reports it rather than rewriting it.

Parsed by `boss_jobs::seed_loader::load_step_plugins`, which refuses a
file named for a kind it does not hold, a status other than `active`,
and a version below 1. `created_at` is not declared here — it is
stamped by the seed's clock when the row is written — and neither is
`authoring_job_id`, which is the packet an operator authored a row
FROM; a bundle row is authored in the tree.
