# Platform cadence bundle — the schedule's BASELINE as data

One file per rule: `<name>.toml` holds exactly one `[[cadence_rule]]`
whose `name` is the file name, carrying every column of the
`cadence_rules` row (`114-cadence-rules.sql`, widened by
`202608282135` for the calendar basis): `name`, `version`, `status`,
`verb`, `basis`, and the basis's own columns — `every_minutes` (wall),
`at_times` (clock, calendar), `min_dock_depth` + `cooldown_minutes`
(queue-depth), `cadence` + `anchor_date` + `business_calendar`
(calendar). A column the basis does not use is simply absent — TOML has
no null. **Adding a rule is dropping a file in.** Changing one is
bumping its `version`.

Since 2026-09-18 (backlog 393d3234, consolidation H4, car 3) this
directory is where a cadence rule is DECLARED. Before it, nine
migrations were the only home — 114 seeded three rules, 202608282140 a
fourth, and 123 / 131 / 134 / 147 / 202609031515 / 202609032030 /
202609042110 each re-versioned one (six of the seven the boarding
threshold; one of them a silent no-op, which is what taught the
retire-by-name idiom). Those files stay exactly as they are
(migrations are append-only history); what changed is that
`infra/lint/migrations-declare-schema-only.sh` refuses `INSERT INTO
cadence_rules` in any migration newer than its cutover stamp, and
`the_cadence_rules_bundle_is_the_migrations_pg.rs` holds this directory
equal, column for column, to the active rows those migrations produce
— and proves the seed can rebuild an emptied table from this directory
alone.

**Three files for four migration rows, by decision.** `protocol-retro-
daily` (202608282140) has no file here: the weekly department retro
opens IT's protocol-retro through a dispatcher clock rule (the
`retro.open` handler, landed the same day), so the daily cadence row is
redundant and the operator retires it by name — and a bundle must not
carry a row the operator is retiring, or the seed would argue with that
decision on every boot. The migrations still insert it on a fresh
database (they are history and still run); the exclusion is written
down as `RETIRED_BY_DECISION` in the equality pin, which compares this
directory to the migrations' active rows MINUS that list, and refuses a
name that leaves the list without a file appearing here.

**The bundle is the baseline, not a lock.** `cadence_rules` is live,
editable protocol data by design (docs/design/protocol-cadence.md:
measure, experiment, update — without a deploy). An operator who
publishes a later version live leaves this file behind, and the seed's
report says so — `behind the live lineage (vN active), untouched` — as
information, never a refusal. The conductor's loop reads the LIVE
active rows over `/api/cadence/rules` and is untouched by this bundle.

**So bump from the LIVE newest, never from the file** (backlog
5449111c). A file behind live turns "bump the version" into a version
that already exists live with other columns: on 2026-09-21 the boarding
cooldown was bumped 6 -> 7 from this directory while live was already
at v7, gated green, landed, and changed nothing. `boss orient`'s
BUNDLES section says "bundle vN, live vM" for every file here, and
`boss gate` refuses to launch a car that changes a row the live lineage
will not take as written — naming the version that would publish.

**Published by `boss-platform-workflow-seed`**, the same binary and
the same `--seed-path` every launcher already passes, after the
workflows, stations and step plugins: the cadence bundle is the
`cadence` directory beside the Workflow bundle (`--cadence-path`
overrides). Insert-if-missing by (name, version), through
`boss_jobs::cadence_seed::seed_cadence_rules` — one decision table for
every bundle, `boss_jobs::bundle_seed`:

| live registry                                   | outcome                 |
|-------------------------------------------------|-------------------------|
| no row of this name                             | insert, active          |
| rows of this name, NONE active                  | retired — left alone    |
| active row at a LOWER version                   | publish; retire the old |
| row at (name, version) active and EQUAL         | present — untouched     |
| row at (name, version) active and DIFFERENT     | REFUSED, by field       |
| row at (name, version) exists but is not active | superseded — untouched  |
| no row at (name, version); active row is HIGHER | behind — untouched      |

The refusal is the one every bundle has: a file edited without a
version bump would otherwise disagree silently with every deployment
that already has the row. The second line matters most here: a rule an
operator RETIRED (switched the schedule off) stays retired — a boot
never switches it back on; re-activation is an explicit publish.

Parsed by `boss_jobs::seed_loader::load_cadence_rules`, which refuses a
file named for a rule it does not hold, a status other than `active`,
and a version below 1. The per-basis column rules are the table's own
CHECK constraint, enforced when the row is written: a file that gives
a `wall` rule an `at_times` fails the boot that tries to publish it,
naming the rule. `created_at` is not declared here — it is stamped by
the seed's clock when the row is written.
