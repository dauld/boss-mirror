# Platform station bundle — stations as DATA, not migrations

One file per station: `<name>.toml` holds exactly one `[[station]]`
whose `name` is the file name, carrying every column of the `stations`
row (`116-stations.sql` and the columns later migrations added:
`terminal_window_days`, `upstream`, `lens`). **Adding a station is
dropping a file in.** Changing one is bumping its `version`.

Since 2026-09-18 (backlog 393d3234, consolidation H4) this directory
is where a platform station is DECLARED. Before it, seven migrations
were the only home — 116 seeded two rows, 118 and 124 one each, 130 /
133 / 20260910201453 bumped versions, 20260915023614 authored one over
its projection, and 119 / 138 / 139 / 202609101900 UPDATEd columns in
place. Those files stay exactly as they are (migrations are append-only
history); what changed is that `infra/lint/migrations-declare-schema-
only.sh` refuses `INSERT INTO stations` in any migration newer than its
cutover stamp, and `the_stations_bundle_is_the_migrations_pg.rs` holds
this directory equal, column for column, to the active rows those
migrations produce — and proves the seed can rebuild an emptied table
from this directory alone.

**Published by `boss-platform-workflow-seed`**, the same binary and
the same `--seed-path` every launcher already passes: the station
bundle is the `stations` directory beside the Workflow bundle
(`--stations-path` overrides). Insert-if-missing by (name, version),
through `boss_jobs::station_seed::seed_stations`:

| live registry                                   | outcome                 |
|-------------------------------------------------|-------------------------|
| no row of this name                             | insert, active          |
| active row at a LOWER version                   | publish; retire the old |
| row at (name, version) active and EQUAL         | present — untouched     |
| row at (name, version) active and DIFFERENT     | REFUSED, by field       |
| row at (name, version) exists but is not active | superseded — untouched  |
| no row at (name, version); active row is HIGHER | behind — untouched      |

The refusal is the point: a file edited without a version bump would
otherwise disagree silently with every deployment that already has
the row. Live authoring (`POST /api/stations/{name}/publish`) is
untouched — an operator's later version supersedes the file's, and the
seed reports it rather than rewriting it.

Parsed by `boss_jobs::seed_loader::load_stations`, which runs every row
through the same viability gate an API publish passes and refuses a
file named for a station it does not hold, a status other than
`active`, and a version below 1. `created_at` is not declared here: it
is stamped by the seed's clock when the row is written.
