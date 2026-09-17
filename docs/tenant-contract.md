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
  and locations batch endpoints' row types, the ledger's
  `chart::AccountInput` + `validate` for the chart of accounts, the
  agents and sensors loaders in `boss_jobs`, the posting-rule and
  projection loaders in `boss_ledger`, `boss_people::Employee`,
  `boss_core`'s `BusinessCalendar` and the gateway's `TenantToml` — and reports one
  line per file: **OK** / **MISSING** (a required file) / **INVALID**
  (with the loader's own error, never rephrased) / **UNKNOWN** (a file
  the contract does not name). Exit 0 when nothing is MISSING or
  INVALID; UNKNOWN is reported, not refused.
- `boss tenant publish <dir> [--gateway <url>] [--dry-run]` publishes
  a directory into a running deployment through the **same shared
  doors the tenant engines' prepare compose** (backlog `ee7b62bb`):
  classes → the chart of accounts → locations → business calendars →
  the company Subject → policy grants → people (two passes) → agents →
  Workflows, after a barrier on the people projection → sensors → the
  ledger's posting rules → its event→fact projections last. Idempotent (insert-if-absent, upsert, 409 swallowed, a
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
| `seeds/chart_of_accounts.toml` | no | POST /api/ledger/accounts/batch, one boss-ledger `chart::AccountInput` per row (insert-if-absent by code) — sent by `boss tenant publish` AFTER the classes; a code the starter chart (40-ledger.sql, the OSS default) already holds is the SAME account, kept under its registered name, and the publish line names the field the declaration differs on — adopt the code or choose another (backlog 41af5195; design 18cf4272) | `[[account]]` rows: code, name, kind (asset|liability|equity|revenue|expense), normal_balance (debit|credit), parent? (a code declared earlier in the file) — the `gl_accounts` table's authorable columns; validated by `boss_ledger::chart::validate` | yes |
| `seeds/employees.json` | no | POST /api/people, one `boss_people::Employee` per row (the brewery engine's prepare reads it at the FIXED path /opt/boss/examples/brewery/seeds/, not from the bundle; used-device-shop reads data/employees.json instead) | JSON array of Employee rows: id, name, email, role, department, hire_date, location, manager_id, employment_type, status, skills[], certifications[], annual_salary_cents; role/department/location are validated against the registries at write time, not here | yes |
| `seeds/operator_hires.toml` | no | boss-brewery-engine prepare (`seed_brewery_operator_hires`): each `[[hire]]` POSTed to /api/people as a `boss_people::Employee` | `[[hire]]` rows in the Employee shape above | no |
| `seeds/business_calendars.json` | no | POST /api/calendar/business-calendars/batch as `Vec<boss_core::calendar::BusinessCalendar>` (the brewery engine's prepare); the dispatcher's timing triggers and the sim resolve business days from it | JSON array of {code, name, weekend: [0..6 Mon=0], closed: [YYYY-MM-DD]} | yes |
| `seeds/sensors.toml` | no | POST /api/sensors/batch (boss-jobs, insert-if-absent by id) — sent by `boss tenant publish` as the tenant's declarations; the dispatcher's `sensor.poll` handler reads the registry every 5 minutes and polls each due sensor (design 14c9b2ad); a push-only source is never due — the gateway's site surface records one `www-visits` reading per page view through POST /api/sensors/{id}/readings (backlog 0b5c5081) | `[[sensor]]` rows: id, source (`stripe` polled; `site` push-only), credential (a `credentials` registry id; none on a push-only source), every_minutes (none on a push-only source), opens (the workflow kind one reading opens), subject_kind, enabled? — validated by `boss_jobs::sensors::load_sensors_toml` | yes |
| `seeds/agents.toml` | no | POST /api/agents/batch (boss-jobs, insert-if-absent by id and by alias) — sent by `boss tenant publish` BEFORE the Workflows (a step's audience may name an agent); a row the platform already registered is kept and the publish line names any field the declaration differs on; the jobs API's login door resolves each alias to the id (design 6fda05ae; backlog f56155f0) | `[[agent]]` rows: id (`agent-<slug>`), display_name, default_model (a rate-card model, e.g. `opus-5[1m]`), aliases? (the logins that sign as it), hourly_budget_usd_micros?, max_concurrent_runs? — the `agents` table's columns and nothing else; validated by `boss_jobs::agents::load_agents_toml` | yes |
| `seeds/posting_rules.toml` | no | POST /api/ledger/posting-rules/batch (boss-ledger, insert-if-absent by fact_kind + version, source = tenant:<id>) — sent by `boss tenant publish` AFTER the Workflows; the posting path evaluates a fact by the newest registry rule for its kind and by the code rules otherwise (backlog a40541cb) | `[[posting_rule]]` rows: fact_kind, version? (1), basis (cash|accrual), lines = [{account_code, side (debit|credit), amount_path (a JSON pointer into the fact payload, integer cents), memo?}] — the debit pointers and the credit pointers must be the same multiset (balanced for every fact); validated by `boss_ledger::posting_rules::load_posting_rules_toml` | yes |
| `seeds/fact_projection_rules.toml` | no | POST /api/ledger/fact-projection-rules/batch (boss-ledger, insert-if-absent by event_kind + when) — sent by `boss tenant publish` after the posting rules; the ledger's facts rebuild projects every matching audit_log event into a financial_fact (backlog a40541cb) | `[[projection]]` rows: event_kind (an audit_log kind), when? (a table of {"/pointer" = value}, every pointer equal for the rule to fire), fact_kind, source_table, source_id_path, happened_on_path?, created_by_path? — the `gl_fact_projection_rules` columns; validated by `boss_ledger::posting_rules::load_projection_rules_toml` | yes |
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

## The chart of accounts is the tenant's

`seeds/chart_of_accounts.toml` (backlog `41af5195`; design `18cf4272`,
David 2026-09-17) declares what the tenant's books are made of: one
`[[account]]` per row of the ledger's `gl_accounts` table — `code`,
`name`, `kind` (`asset` | `liability` | `equity` | `revenue` |
`expense`), `normal_balance` (`debit` | `credit`) and an optional
`parent` naming a code declared earlier in the file. Until this file
had a door, `gl_accounts` came from the product: 40-ledger.sql seeds
the brewery's chart (1000 Cash … 2200 Deferred Revenue, the excise
accounts) and `GET /api/ledger/accounts` was the only way in, so a
tenant that is not a brewery could not name its own accounts without a
migration. The starter chart stays as the **OSS default** the demo
tenant runs on; a tenant's chart is published over it, insert-if-absent
by code, through `POST /api/ledger/accounts/batch` — the batch-door
shape the classes, locations and agents doors share — and every row
the batch inserts leaves one `ledger.account.declared` fact on the
outbox, in the insert's own transaction.

**A code that collides with the starter chart is the same account.**
The code is what every posting rule and journal line points at (`1000`
is debited by name), so a tenant's `1000 Bank` and the starter's
`1000 Cash` cannot be two rows. Insert-if-absent keeps the registered
row under the starter's name and the answer names the difference —
`kept: [{code: "1000", differs: ["name"]}]`, rendered in the publish
line as `1000 (name differs)` — and the tenant then either adopts the
code as it stands or chooses another. Nothing renames an account in
place: a rename would silently re-label every entry already posted.

The worked example is Algedonic's own chart (a tenant commit, not the
product's):

```toml
[[account]]
code = "1000"
name = "Bank"
kind = "asset"
normal_balance = "debit"

[[account]]
code = "1010"
name = "Stripe balance"
kind = "asset"
normal_balance = "debit"
parent = "1000"

[[account]]
code = "2100"
name = "Accounts payable"
kind = "liability"
normal_balance = "credit"

[[account]]
code = "3000"
name = "Owner's equity"
kind = "equity"
normal_balance = "credit"

[[account]]
code = "3100"
name = "Retained earnings"
kind = "equity"
normal_balance = "credit"

[[account]]
code = "4100"
name = "Sponsorship revenue"
kind = "revenue"
normal_balance = "credit"

[[account]]
code = "4200"
name = "Support revenue"
kind = "revenue"
normal_balance = "credit"

[[account]]
code = "4300"
name = "Hosting revenue"
kind = "revenue"
normal_balance = "credit"

[[account]]
code = "6100"
name = "Payment processing fees"
kind = "expense"
normal_balance = "debit"

[[account]]
code = "6200"
name = "Infrastructure"
kind = "expense"
normal_balance = "debit"

[[account]]
code = "6300"
name = "Hardware"
kind = "expense"
normal_balance = "debit"

[[account]]
code = "6400"
name = "Software and services"
kind = "expense"
normal_balance = "debit"
```

Published against the starter chart, nine of these twelve codes
collide — `1000` Cash, `1010` Cash in Transit, `2100` Accounts Payable,
`3000` Retained Earnings, `4100` Revenue — Wholesale Beer Sales, `6100`
Operating Expense — Payroll, `6200` Operating Expense — Rent, `6300`
Operating Expense — General, `6400` Payroll Taxes & Benefits — and the
publish line reads `received 12, inserted 3; kept as registered, not as
declared — adopt the code or choose another: 1000 (name differs); 1010
(name differs); …`. That line is the decision surface: each kept code
is either adopted (the tenant posts to `1000` and calls it what the
ledger calls it) or moved (the tenant picks `1001 Bank` and the next
publish inserts it). `boss tenant check` cannot see the deployment's
chart, so it judges the file alone: codes unique, kinds and balances
inside the enum, every parent declared before its child.

## The ledger's two rule files — a worked example

A company's own revenue line needs one more posting rule than the
product ships, and until backlog `a40541cb` (2026-09-17) that meant a
fork of `boss-ledger`'s `BossRuleSet`. Two files now carry it as data,
published by `boss tenant publish` after the sensors: posting rules
land in the `gl_posting_rules` registry (`POST
/api/ledger/posting-rules/batch`, insert-if-absent by fact_kind +
version, `source = tenant:<id>`), projections in
`gl_fact_projection_rules` (`POST /api/ledger/fact-projection-rules/batch`,
insert-if-absent by event_kind + `when`). Both are append-only: a
changed rule is the next `version`, a changed projection is a new row.
Each row landed records a `ledger.posting_rule.declared` /
`ledger.fact_projection_rule.declared` fact with `declared_by`.

Algedonic's sponsorship receipt is the first case. The
`receive-a-sponsorship` workflow's `recognize` step is a `task`, so its
completion is a `step.done.task` event — the same kind every task step
of every workflow emits. **Measured shape of that event** (boss-jobs,
`http/steps.rs`): `{job_id, step_id, kind, subject_kind, subject_id,
workflow_kind, completed_on, metadata, notify_on_done, spec_slug}`.
`jobs.step.completed` — the packet's first guess — carries only
`{job_id, step_id}` and cannot name a workflow; `workflow_kind` (the
parent job's kind) was added to `step.done.<kind>` by this change
beside `spec_slug` (the step's slug), and those two are the pointers a
projection filters on. The amounts live in the step's `metadata`, so
the recognize step's `fields` carry `amount_cents` and `fee_cents` (a
`string` field is integer cents; the ledger reads a numeric string):

```toml
# seeds/fact_projection_rules.toml
[[projection]]
event_kind = "step.done.task"
when = { "/workflow_kind" = "receive-a-sponsorship", "/spec_slug" = "recognize" }
fact_kind = "finance.sponsorship.received"
source_table = "jobs"
source_id_path = "/job_id"          # one fact per job: the id is UUIDv5 over (kind, source_table, source_id)
happened_on_path = "/completed_on"  # the step's completion date, YYYY-MM-DD

# seeds/posting_rules.toml — cash basis, recognized on receipt
# (David, 2026-09-16: a sponsorship is a gift, no term, no obligation)
[[posting_rule]]
fact_kind = "finance.sponsorship.received"
basis = "cash"
lines = [
  { account_code = "1010", side = "debit",  amount_path = "/metadata/amount_cents", memo = "Sponsorship {/job_id}" },
  { account_code = "4100", side = "credit", amount_path = "/metadata/amount_cents" },
  { account_code = "6100", side = "debit",  amount_path = "/metadata/fee_cents",    memo = "Stripe fee" },
  { account_code = "1010", side = "credit", amount_path = "/metadata/fee_cents" },
]
```

`/metadata/amount_cents` appears once on each side and so does
`/metadata/fee_cents`, which is what makes the rule admissible: the
entry balances for every fact, not for the one someone tried. A fact
`{amount_cents: 100, fee_cents: 33}` posts DR 1010 100 / CR 4100 100 /
DR 6100 33 / CR 1010 33 with the memo `finance.sponsorship.received —
posting rule v1`; a fee of 0 omits both fee lines. A rule that drops
the last line is refused by `check` (INVALID, naming
`finance.sponsorship.received v1`) and by the door (422) with the same
words. The files above are the tenant's commit, not the product's;
`boss tenant init` writes both as commented templates.

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
