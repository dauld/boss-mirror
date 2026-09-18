# Platform delivery-policy bundle — what the conductor decides by, as data

One file per policy: `<name>.toml` holds exactly one `[[delivery_policy]]`
whose `name` is the file name, carrying every column of the
`delivery_policy` row (`202608242117-delivery-policy-registry.sql`,
widened by `202609030800` and `202609031000`, narrowed by
`20260918102236`): `name`, `version`, `status`, `max_red_trains`,
`stall_hours`, `consist_budget_secs`, `consist_output_budget`,
`consist_files_named`, `skip_reason_file_budget`, `blip_cause_budget`,
`ci_host_floor_gb`, `gate_max_concurrent`. There is one policy,
`train-conductor`, because there is one delivery pipeline; **the
version is what moves.** Changing the policy is bumping its `version`.

Since 2026-09-18 (backlog 393d3234, consolidation H4, car 4 of 4 — the
last registry to make this move) this directory is where the delivery
policy is DECLARED. Before it, two migrations were the only home —
202608242117 seeded v1 as train.rs's compiled constants carried over
verbatim, and 202609050500 retired it and published v2 with the CI host
floor at 40 (two more widened the table with a default each, and one
dropped a column). Those files stay exactly as they are (migrations are
append-only history); what changed is that
`infra/lint/migrations-declare-schema-only.sh` refuses `INSERT INTO
delivery_policy` in any migration newer than its cutover stamp, and
`the_delivery_policy_bundle_is_the_migrations_pg.rs` holds this
directory equal, column for column, to the active row those migrations
produce — and proves the seed can rebuild an emptied table from this
directory alone.

**One row is the whole policy** — the `workflows` shape, not the
`cadence_rules` shape. A Job pins one workflow version; a train pins
one policy version (`metadata.delivery_policy_version`, stamped at
boarding) and reconcile judges that train by the version it departed
under, so a bump here never rewrites the rules a train in flight left
on. The reader is `crates/orchestrators/boss-cli/src/delivery_policy.rs`,
over `/api/delivery/policy/train-conductor`; `train.rs` is what it
DOES with the answer.

**The compiled fallback stays, and is held equal to this file.** The
conductor runs outside the cluster and must board trains when the
registry is unreachable, empty, or holds a row that does not parse — a
policy registry must not become a new way to wedge every train — so
`DeliveryPolicy::compiled()` carries the same numbers and a train that
departs on it pins no version. `the_bundle_equals_the_compiled_fallback`
(boss-cli, no database) refuses the two drifting; the older
`the_seeded_policy_equals_the_compiled_fallback` (Postgres-backed)
closes the triangle through the migrations. The `COMPILED_*` constants
are not redundant while the conductor has no tree to read at runtime;
`estate_compare.rs` also reads one of them out of the source text.

**Published by `boss-platform-workflow-seed`**, the same binary and
the same `--seed-path` every launcher already passes, after the
workflows, stations, step plugins and cadence rules: the delivery-policy
bundle is the `delivery-policy` directory beside the Workflow bundle
(`--delivery-policy-path` overrides). Insert-if-missing by (name,
version), through
`boss_jobs::delivery_policy_seed::seed_delivery_policies` — one decision
table for every bundle, `boss_jobs::bundle_seed`:

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
that already has the row. A lineage with no active row is one an
operator retired (the conductor then runs on the compiled fallback and
says so in its journal) and a boot leaves it that way; re-activation is
an explicit publish.

Parsed by `boss_jobs::seed_loader::load_delivery_policies`, which
refuses a file named for a policy it does not hold, a status other than
`active`, a version below 1, and a column the table does not have. The
positive-value rules are the table's own CHECK constraints, enforced
when the row is written, and the conductor's parser refuses the same
values a second time on read — it reads over HTTP and must not depend
on having been served by the database it thinks it was. `created_at` is
not declared here — it is stamped by the seed's clock when the row is
written.
