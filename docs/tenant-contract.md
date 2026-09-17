# The tenant contract — what BOSS reads from a tenant directory

A **tenant is a directory of its own**, outside the product tree, in
exactly the shape of `examples/<tenant>/` in this repo: `tenant.toml`
(the manifest) plus `seeds/`. The product's two examples —
`examples/brewery` and `examples/used-device-shop` — exercise this
contract publicly; a company's own tenant (Algedonic, LLC's is the
private repo `david/algedonic-llc`, the first real customer) lives in
its own repository and is never part of the product. Decided with
David on 2026-09-16 (backlog `fcc1d57b`): *"someone downloading BOSS
to adapt it wants a more natural home for their adaption"* than a
patch to our examples.

Three verbs make the contract usable:

- `boss tenant init <name> [--into <dir>]` writes a new tenant
  directory from the contract — every scaffolded file valid, minimal
  and commented, with a README naming each file and its reader. It
  refuses a non-empty target.
- `boss tenant check <dir>` validates a directory **with the product's
  own loaders** — `boss_jobs::seed_loader` for `workflows.toml` (with
  its viability lint), `boss_policy_client`'s grant loader, the classes
  and locations batch endpoints' row types, the agents and sensors
  loaders in `boss_jobs`, `boss_people::Employee`,
  `boss_core`'s `BusinessCalendar` and the gateway's `TenantToml` — and reports one
  line per file: **OK** / **MISSING** (a required file) / **INVALID**
  (with the loader's own error, never rephrased) / **UNKNOWN** (a file
  the contract does not name). Exit 0 when nothing is MISSING or
  INVALID; UNKNOWN is reported, not refused.
- `boss tenant publish <dir> [--gateway <url>] [--dry-run]` publishes
  a directory into a running deployment through the **same shared
  doors the tenant engines' prepare compose** (backlog `ee7b62bb`):
  classes → locations → business calendars → the company Subject →
  policy grants → people (two passes) → agents → Workflows, after a
  barrier on the people projection → sensors last. Idempotent (insert-if-absent, upsert, 409 swallowed, a
  kind an authoring Job already published is skipped), signed as
  `automation:tenant-seed` and **not** as a sim chain. One line per
  file present: the door and a count, or `skipped: <why>` for a file
  nothing reads — never silence. It refuses a directory that fails
  `check` BEFORE the first write, plan still printed; `--dry-run`
  prints the plan and makes no HTTP call. This is how a tenant with no
  engine — the real company — gets published; the container launcher
  (`infra/oss-quickstart/tenant-launch.sh`) chooses it for any tenant
  whose `[meta] tenant_id` is not the brewery's, via
  `infra/seed-tenant.sh`, and stamps no sim baseline.

`boss tenant contract` prints the table below. It is rendered from
`CONTRACT` in `crates/orchestrators/boss-cli/src/tenant.rs`, and a test
(`the_contract_doc_carries_the_codes_table`) holds this file's copy
equal to the code's — one fact in two places gets an equality test
(CLAUDE.md §9a). Edit the code, run the verb, paste between the markers.

## How the table was made

Measured on 2026-09-16 by grepping every reader of
`examples/<tenant>/seeds/*` across `crates/` and `infra/` — the seed
loader, the gateway's manifest reader, the policy bootstrap, the two
tenant engines' prepare steps, the seed shell scripts, the gate's
scoping rules and the live-protocols lint. Three findings worth
stating plainly:

- **Two shipped files had no reader; one still has none.**
  `seeds/locations.toml` and `seeds/subject_kinds.toml` are authored
  in the brewery bundle and, when measured, nothing consumed either:
  no seeder POSTed locations, so an `employees.json` row's `location`
  had to already exist in the registry — which is how the first real
  tenant's founder came to sit at the platform's `loc-hq` (backlog
  `1ec8312a`). On 2026-09-17 locations gained their door, `POST
  /api/locations/batch` (insert-if-absent by id), sent by `publish`
  before the roster; `subject_kinds.toml` still has none, the table
  says "NO READER" rather than implying one, and `check` parses it
  conservatively.
- **The manifest has two readers with two strictnesses.** The gateway
  reads `[meta]`/`[modules]`/`[labels]` and ignores the rest; the
  simulator's `TenantConfig` additionally requires `seed`,
  `start_date`, `end_date` and the `[job_rates]` blocks — sim-only
  sections that only a tenant with an engine carries. The contract is
  the platform's (gateway) reading; `check` requires `[meta]
  tenant_id` because it is every workflow's `owning_team`.
- **Half the brewery bundle is its engine's data, not the platform's.**
  `accounts.toml`, `vendors.toml`, `messages.toml` and
  `bulletins.toml` are `include_str!`'d into `boss-brewery-engine` at
  compile time from `examples/brewery/seeds` — a copy in a tenant
  directory is never read. `excise_rates.toml` and `parts.toml` are
  read from the bundle by that engine only; `products.toml` is read by
  nothing (the engine hardcodes the catalog and asks to keep them in
  sync). They are in the table so the brewery passes with no UNKNOWN
  rows, marked as engine data; `init` does not write them.

## The contract

<!-- contract-table:begin -->
| file | required | read by | shape | `init` writes it |
|---|---|---|---|---|
| `tenant.toml` or `seeds/tenant.toml` | yes | boss-gateway `/api/tenant/manifest` (+ inlined into index.html) via `boss_core::tenant_manifest::TenantToml`, path `BOSS_TENANT_MANIFEST_TOML`; boss-sim `TenantConfig` reads the same file with sim-only sections (`seed`, `start_date`, `[job_rates]`) for a tenant that has an engine | `[meta] tenant_id` (required by check: it is every workflow's `owning_team`), `display_name`; `[modules] <module> = bool`; `[labels] <dotted.key> = str` | yes |
| `seeds/workflows.toml` | yes | boss-jobs `seed_loader::load_workflows_with_owning_team` + the viability lint (the tenant prepare publishes each row); infra/lint/the-live-protocols-are-the-authored-protocols.sh; infra/gcp/publish-workflow.sh | `[[workflow]]` rows (kind, label, category, subject_kinds, description, metadata) each with flat `[[workflow.step]]` rows whose `ready_when` predicates imply the DAG; >= 1 trigger and >= 1 terminal per workflow | yes |
| `seeds/policy_rules.toml` | no | boss-policy-bootstrap / `boss_policy::bootstrap::publish_policy_rules` via `boss_policy_client::seed_loader::load_policy_rules` (the tenant prepare, first boot) | `[[grants]]` rows: `role` or `roles`, `resource` or `resources`, `action` or `actions`, `scope` (all/self/team/territory/none/department:<name>); expanded to one rule per role x resource x action | yes |
| `seeds/classes.json` or `seeds/classes.toml` | no | POST /api/classes/batch, one boss-classes `http::ClassInput` per row — sent by the tenant prepare (brewery: classes.json; used-device-shop: classes.toml `[[class]]`) and infra/postgres/reset-to-baseline.sh | JSON array (or TOML `[[class]]` rows) of {subject_kind, code, display_name, parent_code?, member_attribute?, metadata?, sort_order?} | yes |
| `seeds/employees.json` | no | POST /api/people, one `boss_people::Employee` per row (the brewery engine's prepare reads it at the FIXED path /opt/boss/examples/brewery/seeds/, not from the bundle; used-device-shop reads data/employees.json instead) | JSON array of Employee rows: id, name, email, role, department, hire_date, location, manager_id, employment_type, status, skills[], certifications[], annual_salary_cents; role/department/location are validated against the registries at write time, not here | yes |
| `seeds/operator_hires.toml` | no | boss-brewery-engine prepare (`seed_brewery_operator_hires`): each `[[hire]]` POSTed to /api/people as a `boss_people::Employee` | `[[hire]]` rows in the Employee shape above | no |
| `seeds/business_calendars.json` | no | POST /api/calendar/business-calendars/batch as `Vec<boss_core::calendar::BusinessCalendar>` (the brewery engine's prepare); the dispatcher's timing triggers and the sim resolve business days from it | JSON array of {code, name, weekend: [0..6 Mon=0], closed: [YYYY-MM-DD]} | yes |
| `seeds/sensors.toml` | no | POST /api/sensors/batch (boss-jobs, insert-if-absent by id) — sent by `boss tenant publish` as the tenant's declarations; the dispatcher's `sensor.poll` handler reads the registry every 5 minutes and polls each due sensor (design 14c9b2ad) | `[[sensor]]` rows: id, source (`stripe`), credential (a `credentials` registry id), every_minutes, opens (the workflow kind one reading opens), subject_kind, enabled? — validated by `boss_jobs::sensors::load_sensors_toml` | yes |
| `seeds/agents.toml` | no | POST /api/agents/batch (boss-jobs, insert-if-absent by id and by alias) — sent by `boss tenant publish` BEFORE the Workflows (a step's audience may name an agent); a row the platform already registered is kept and the publish line names any field the declaration differs on; the jobs API's login door resolves each alias to the id (design 6fda05ae; backlog f56155f0) | `[[agent]]` rows: id (`agent-<slug>`), display_name, default_model (a rate-card model, e.g. `opus-5[1m]`), aliases? (the logins that sign as it), hourly_budget_usd_micros?, max_concurrent_runs? — the `agents` table's columns and nothing else; validated by `boss_jobs::agents::load_agents_toml` | yes |
| `seeds/locations.toml` | no | POST /api/locations/batch, one boss-locations `http::LocationInput` per row (insert-if-absent by id) — sent by `boss tenant publish` BEFORE the roster, because an `employees.json` `location` is a foreign key into the registry (backlog 1ec8312a; until 2026-09-17 nothing read this file) | `[[location]]` rows: id, name, kind, timezone (+ parent_id, latitude, longitude, address, account_id, metadata) — the `locations` table's columns | yes |
| `seeds/subject_kinds.toml` | no | NO READER (measured 2026-09-16). Check parses the rows conservatively | `[[subject_kind]]` rows: kind, label, description, owning_team, sort_order | no |
| `seeds/accounts.toml` | no | boss-brewery-engine, `include_str!` at compile time from examples/brewery/seeds — a copy in a tenant directory is never read | brewery engine data (`names`, `[[city]]`); check parses TOML only | no |
| `seeds/vendors.toml` | no | boss-brewery-engine, `include_str!` at compile time — never read from a tenant directory | brewery engine data (`[[vendor]]`); check parses TOML only | no |
| `seeds/messages.toml` | no | boss-brewery-engine, `include_str!` at compile time — never read from a tenant directory | brewery engine data (`[[thread]]`); check parses TOML only | no |
| `seeds/bulletins.toml` | no | boss-brewery-engine, `include_str!` at compile time — never read from a tenant directory | brewery engine data (`[[bulletin]]`); check parses TOML only | no |
| `seeds/excise_rates.toml` | no | boss-brewery-engine prepare (`seeds_dir/excise_rates.toml`) | brewery engine data (`effective_from`, `[[schedule]]`); check parses TOML only | no |
| `seeds/parts.toml` | no | boss-brewery-engine `load_parts` (raw-materials catalog + opening balances) | brewery engine data (`[[parts]]`); check parses TOML only | no |
| `seeds/products.toml` | no | NO READER (measured 2026-09-16): the brewery's finished-product catalog is hardcoded in its prepare, which says to keep it in sync with this file | brewery engine data (`[[products]]`); check parses TOML only | no |
<!-- contract-table:end -->

## Two spellings the table accepts

- `tenant.toml` at the root is the canonical place (the real tenant's
  shape). `seeds/tenant.toml` is accepted because the examples keep it
  there — and because a DELIVERED tenant arrives that way: the
  deployment names one directory, `BOSS_TENANT_DIR` on the boss
  container (`infra/cluster/manifests/boss.yaml`; car 2 of `fcc1d57b`,
  backlog `f4f5c387`), and the launcher
  (`infra/oss-quickstart/services-launcher.sh`) derives
  `BOSS_TENANT_MANIFEST_TOML` from it — `tenant.toml` at the root,
  else `seeds/tenant.toml` — along with `BOSS_SIM_SEEDS_DIR`. A
  `tenant_repo` instance's tenant is delivered by the converge as ONE
  flat ConfigMap mounted at `/opt/boss/tenant/seeds`, its root
  `tenant.toml` riding as `seeds/tenant.toml` (a mount inside a
  read-only mount cannot be created), so both spellings stay.
- `seeds/classes.json` (brewery) or `seeds/classes.toml` (`[[class]]`
  rows, used-device-shop): the same rows, two formats, one endpoint.

## What is outside the contract

- **Prose** — `README.md`, `DOMAIN.md` — is not judged.
- **`data/`** — the tenant engines' bulk data (`catalog.json`,
  `assets.json`, `marketing-assets.json`, and used-device-shop's
  `data/employees.json`) is read by those engines from `seeds/../data`
  and is not part of the platform contract; `check` does not descend
  into it.
- **A field the registry cannot hold.** `seeds/agents.toml` was the
  first extension the real tenant asked for and read UNKNOWN until its
  seed landed (backlog `f56155f0`, 2026-09-17). Its row is the `agents`
  table's columns plus the aliases the row owns, and nothing else: the
  real tenant's first draft also carried `role` and `department`, which
  the registry has no column for, and `check` refuses those **by name**
  (`unknown field`) rather than dropping them in silence. A row the
  platform already registered (prod's `agent-claude` came from
  migration `20260915212644`) is kept as registered, and the publish
  line names any field the declaration differs on.

## What `check` found on the first real tenant

Run against the shape of `david/algedonic-llc` @ `7619cfc` (the
fixture in `tenant.rs`'s tests, copied so no test reads the private
repo):

- `seeds/workflows.toml`'s `receive-a-sponsorship` declares its
  trigger as `trigger_kind = "sensor"`; the `trigger` StepType admits
  `periodic|event|operator|counterparty`, so the viability lint refuses
  the bundle and the publish would too. INVALID, with the lint's own
  line. (Either the tenant says `event`, or a `sensor` trigger kind is a
  registry change — a StepType row, not code.)
- `seeds/policy_rules.toml` is written as `[[rule]]` rows with a `why`;
  the loader reads `[[grants]]` only and so reads **zero rules** from
  it — the founder's "everything" grant would never be seeded. `check`
  reports it INVALID naming both table names. A file that parses to
  nothing is not a pass.
- `seeds/business_calendars.json` is written as `{id, working_days,
  day_start, day_end}`; the batch endpoint's `BusinessCalendar` is
  `{code, name, weekend, closed}` and would reject it. INVALID with
  serde's own "missing field `code`".
- `seeds/agents.toml` — UNKNOWN then; since `f56155f0` it is judged,
  and its `role` / `department` are refused by name (above).
- Everything else — the manifest, the
  classes, the founder's employee row, the location — is OK under the
  product's own loaders.
