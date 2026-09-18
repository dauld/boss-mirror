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
  `boss_core`'s `BusinessCalendar`, the gateway's `TenantToml` and the
  dispatcher's own `[[rule]]` parser + publish gate for `rules.toml` — and reports one
  line per file: **OK** / **MISSING** (a required file) / **INVALID**
  (with the loader's own error, never rephrased) / **UNKNOWN** (a file
  the contract does not name). Exit 0 when nothing is MISSING or
  INVALID; UNKNOWN is reported, not refused.
- `boss tenant publish <dir> [--gateway <url>] [--dry-run] [--take <registry>[,<registry>]]` publishes
  a directory into a running deployment through the **same shared
  doors the tenant engines' prepare compose** (backlog `ee7b62bb`):
  classes → the chart of accounts → locations → business calendars →
  the company Subject → policy grants → people (two passes) → agents →
  Workflows, after a barrier on the people projection → credentials →
  sensors → the
  ledger's posting rules → its event→fact projections → dispatcher
  rules last. Idempotent (insert-if-absent on every door — the
  instance is the truth; a row already there that differs from the
  file is kept and named on the line, and only `--take <registry>`
  overwrites it; a kind an authoring Job already published is kept, a
  rule at its file's version is `present`), signed as
  `automation:tenant-seed` and **not** as a sim chain. One line per
  file present: the door and a count, or `skipped: <why>` for a file
  nothing reads — never silence. It refuses a directory that fails
  `check` BEFORE the first write, plan still printed; `--dry-run`
  prints the plan and makes no HTTP call. This is how EVERY tenant gets
  published: the container launcher
  (`infra/oss-quickstart/tenant-launch.sh`) runs it first for whatever
  directory the instance names, via `infra/seed-tenant.sh`, which
  stamps no sim baseline; a tenant with an engine — the brewery — then
  has its engine's prepare run after it, for the sim data only it
  seeds and the sim's reset baseline (backlog `b644d727`, 2026-09-17:
  until then the brewery reached an instance only through that
  prepare, which never sent its locations, its chart or its rules).

`boss tenant contract` prints the table below and the publish rule
("The instance is the truth"). Both are rendered from
`crates/orchestrators/boss-cli/src/tenant.rs` (`CONTRACT` and
`INSTANCE_IS_THE_TRUTH`), and two tests
(`the_contract_doc_carries_the_codes_table`,
`the_contract_doc_carries_the_publish_rule_the_verb_prints`) hold this
file's copies equal to the code's — one fact in two places gets an
equality test (CLAUDE.md §9a). Edit the code, run the verb, paste
between the markers.

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
- **The container launcher reads `[modules]` too, the SPA's way.**
  infra/oss-quickstart/tenant-modules.sh starts a module's service only
  when the manifest lists that module true — `sim` (the /simulator UX,
  and the brewery tick daemon's `BOSS_SIM_ENABLED` when the deployment
  did not set it), `equipment` (boss-catalog-api, boss-assets-api),
  `warehouse` (boss-inventory-api), `shipping` (boss-shipping-api) —
  and derives the sim's loopback pair (`BOSS_SIM_CALLBACK_BIND`,
  `BOSS_EVENT_WEBHOOK_URL`) only when the sim runs (backlog 18d6a6c9).
  `boss-launch --plan` prints the decision per service. A deployment
  with no manifest path keeps the full roster.
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
| `tenant.toml` or `seeds/tenant.toml` | yes | boss-gateway `/api/tenant/manifest` (+ inlined into index.html) via `boss_core::tenant_manifest::TenantToml`, path `BOSS_TENANT_MANIFEST_TOML`; boss-sim `TenantConfig` reads the same file with sim-only sections (`seed`, `start_date`, `[job_rates]`) for a tenant that has an engine | `[meta] tenant_id` (required by check: it is every workflow's `owning_team`), `display_name` (the tab title and wordmark); `[modules] <module> = bool` — a module is ON only when listed true, a missing key is off (ce68f137); the SPA reads `calendar`, `equipment`, `exec`, `finance`, `marketing-assets`, `parts`, `qa`, `shipping`, `shop`, `sim`, `support`, `warehouse`; `[labels] <dotted.key> = str` | yes |
| `seeds/workflows.toml` | yes | boss-jobs `seed_loader::load_workflows_with_owning_team` + the viability lint (the tenant prepare publishes each row); infra/lint/the-live-protocols-are-the-authored-protocols.sh; infra/gcp/publish-workflow.sh | `[[workflow]]` rows (kind, label, category, subject_kinds, description, metadata) each with flat `[[workflow.step]]` rows whose `ready_when` predicates imply the DAG; >= 1 trigger and >= 1 terminal per workflow | yes |
| `seeds/policy_rules.toml` | no | boss-policy-bootstrap / `boss_policy::bootstrap::publish_policy_rules` via `boss_policy_client::seed_loader::load_policy_rules` (the tenant prepare, first boot) | `[[grants]]` rows: `role` or `roles`, `resource` or `resources`, `action` or `actions`, `scope` (all/self/team/territory/none/department:<name>); expanded to one rule per role x resource x action | yes |
| `seeds/classes.json` or `seeds/classes.toml` | no | POST /api/classes/batch, one boss-classes `http::ClassInput` per row — sent by the tenant prepare (brewery: classes.json; used-device-shop: classes.toml `[[class]]`) and infra/postgres/reset-to-baseline.sh | JSON array (or TOML `[[class]]` rows) of {subject_kind, code, display_name, parent_code?, member_attribute?, metadata?, sort_order?} | yes |
| `seeds/chart_of_accounts.toml` | no | POST /api/ledger/accounts/batch, one boss-ledger `chart::AccountInput` per row (insert-if-absent by code) — sent by `boss tenant publish` AFTER the classes; a code the starter chart (40-ledger.sql, the OSS default) already holds is the SAME account, kept under its registered name, and the publish line names the field the declaration differs on — adopt the code or choose another (backlog 41af5195; design 18cf4272) | `[[account]]` rows: code, name, kind (asset|liability|equity|revenue|expense), normal_balance (debit|credit), parent? (a code declared earlier in the file) — the `gl_accounts` table's authorable columns; validated by `boss_ledger::chart::validate` | yes |
| `seeds/employees.json` | no | POST /api/people, one `boss_people::Employee` per row; a row already there is kept and the publish line names the declared fields that differ — the instance is the truth; `--take employees` PUTs the declared fields and keeps the rest (design e187198f) — sent by `boss tenant publish` (the brewery engine's prepare reads it at the FIXED path /opt/boss/examples/brewery/seeds/, not from the bundle; used-device-shop reads data/employees.json instead) | JSON array of Employee rows: id, name, email, role, department, hire_date, location, manager_id, employment_type, status, skills[], certifications[], annual_salary_cents; role/department/location are validated against the registries at write time, not here | yes |
| `seeds/operator_hires.toml` | no | boss-brewery-engine prepare (`seed_brewery_operator_hires`): each `[[hire]]` POSTed to /api/people as a `boss_people::Employee` | `[[hire]]` rows in the Employee shape above | no |
| `seeds/business_calendars.json` | no | POST /api/calendar/business-calendars/batch as `Vec<boss_core::calendar::BusinessCalendar>` (the brewery engine's prepare and `boss tenant publish`; insert-if-absent by code, a held code that differs is named, `--take calendars` replaces it wholesale — design e187198f); the dispatcher's timing triggers and the sim resolve business days from it | JSON array of {code, name, weekend: [0..6 Mon=0], closed: [YYYY-MM-DD]} | yes |
| `seeds/credentials.toml` | no | POST /api/credentials/batch (boss-jobs, insert-if-absent by id; one `credential.declared` fact per inserted row) — sent by `boss tenant publish` BEFORE the sensors, because a sensor names a credential by id; the broker's rotation handlers and the forge-token audit read the rows it lands. KNOWLEDGE only: where a value lives and who reads it — the value stays in the deployment's Secret, and a key the shape does not name is refused (backlog ee368d0c: until 2026-09-18 these rows were seeded by migrations, so every install carried one operator's credential ids) | `[[credential]]` rows: id, kind (`forgejo-access-token`, `stripe-restricted-key`, ...), issuer, principal, scopes? (as the issuer spells them; empty = unverified), storage_location (a Secret ns/name/key or a file path — never a value), consumers? = [{kind, location}], rotation_policy? (on-demand | scheduled), notes? — the `credentials` table's declarable columns; validated by `boss_jobs::credentials::load_credentials_toml` | yes |
| `seeds/sensors.toml` | no | POST /api/sensors/batch (boss-jobs, insert-if-absent by id) — sent by `boss tenant publish` as the tenant's declarations; the dispatcher's `sensor.poll` handler reads the registry every 5 minutes and polls each due sensor (design 14c9b2ad); a push-only source is never due — the gateway's site surface records one `www-visits` reading per page view through POST /api/sensors/{id}/readings (backlog 0b5c5081) | `[[sensor]]` rows: id, source (`stripe` for succeeded charges and `stripe-payouts` for paid payouts, both polled on the same credential; `site` push-only), credential (a `credentials` registry id; none on a push-only source), every_minutes (none on a push-only source), opens (the workflow kind one reading opens), subject_kind, enabled? — validated by `boss_jobs::sensors::load_sensors_toml` | yes |
| `seeds/agents.toml` | no | POST /api/agents/batch (boss-jobs) — sent by `boss tenant publish` BEFORE the Workflows (a step's audience may name an agent); a row the registry lacks is inserted, a row it holds is kept and the publish line names the declared fields that differ, an alias the tenant does not declare is kept — the instance is the truth; `--take agents` applies the whole declaration and names each change from → to (design e187198f); the jobs API's login door resolves each alias to the id (design 6fda05ae; backlog f56155f0) | `[[agent]]` rows: id (`agent-<slug>`), display_name, default_model (a rate-card model, e.g. `opus-5[1m]`), aliases? (the logins that sign as it), role? and department? (Class codes under (employee, role) / (employee, department), checked against the registry at the batch door like an employee's — a role audience resolves to every holder, agents included; backlog ab192a9f), hourly_budget_usd_micros?, max_concurrent_runs? — the `agents` table's columns and nothing else; validated by `boss_jobs::agents::load_agents_toml` | yes |
| `seeds/posting_rules.toml` | no | POST /api/ledger/posting-rules/batch (boss-ledger, insert-if-absent by fact_kind + version, source = tenant:<id>) — sent by `boss tenant publish` AFTER the Workflows; the posting path evaluates a fact by the newest registry rule for its kind and by the code rules otherwise (backlog a40541cb) | `[[posting_rule]]` rows: fact_kind, version? (1), basis (cash|accrual), lines = [{account_code, side (debit|credit), amount_path (a JSON pointer into the fact payload, integer cents), memo?}] — the debit pointers and the credit pointers must be the same multiset (balanced for every fact); validated by `boss_ledger::posting_rules::load_posting_rules_toml` | yes |
| `seeds/fact_projection_rules.toml` | no | POST /api/ledger/fact-projection-rules/batch (boss-ledger, insert-if-absent by event_kind + when) — sent by `boss tenant publish` after the posting rules; the ledger's facts rebuild projects every matching audit_log event into a financial_fact (backlog a40541cb) | `[[projection]]` rows: event_kind (an audit_log kind whose family the platform event stream ingests — `boss_nats::durable::stream_subjects`; a rule on any other family would fire never live, so it is refused naming the family, backlog 94f20e76), when? (a table of {"/pointer" = value}, every pointer equal for the rule to fire), fact_kind, source_table, source_id_path, happened_on_path?, created_by_path? — the `gl_fact_projection_rules` columns; validated by `boss_ledger::posting_rules::load_projection_rules_toml` | yes |
| `seeds/locations.toml` | no | POST /api/locations/batch, one boss-locations `http::LocationInput` per row (insert-if-absent by id) — sent by `boss tenant publish` BEFORE the roster, because an `employees.json` `location` is a foreign key into the registry (backlog 1ec8312a; until 2026-09-17 nothing read this file) | `[[location]]` rows: id, name, kind, timezone (+ parent_id, latitude, longitude, address, account_id, metadata) — the `locations` table's columns | yes |
| `seeds/rules.toml` | no | POST /api/dispatcher/rules/_validate, then POST /api/dispatcher/rules (a draft carrying `source = tenant:<tenant_id>`) + POST /api/dispatcher/rules/{name}/publish per rule (boss-dispatcher) — sent by `boss tenant publish` LAST, after the Workflows a rule reacts on; append-only: an unchanged version is a no-op (the line says `present`), a higher version supersedes, a live version ahead of the file is left alone, a name another source holds live is refused and a name the product retired is taken over (backlog 70bc5725); the dispatcher's boot seed retires only product-sourced rules no file names, so a tenant's rule survives every converge (backlog 458971ef) | `[[rule]]` rows in the product rule file's own shape (infra/dispatcher/rules/*.toml): name, why, version, on_event or schedule, when?, delay?, `[[rule.do]]` handler + args — parsed by `boss_dispatcher::rules::registry::parse_raw_file`, validated by the publish door's own `authoring::validate`, handler names checked against `cascade::handler_emits` (this build's roster) | yes |
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
  table's columns plus the aliases the row owns, and nothing else, and
  `check` refuses any other key **by name** (`unknown field`) rather
  than dropping it in silence. The real tenant's first draft carried
  `role` and `department` and was refused that way until the registry
  gained the columns (backlog `ab192a9f`): an agent now holds a role and
  sits in a department exactly as an employee does — both Class codes
  under `(employee, role)` / `(employee, department)`, checked at the
  batch door against the same registry an employee's are, so a code
  `seeds/classes.json` does not declare is refused naming the agent,
  the attribute and the code. A step whose audience is `{ role = X }`
  resolves to every holder of X, and an agent holding X is one: the
  dispatcher's nomination reads the people roster and the agents
  registry as one roster. A department is carried and validated but
  routes nothing yet (design `f5ebd2e1` car 2). A row the platform
  already registered (prod's `agent-claude` came from migration
  `20260915212644`) is kept as the instance holds it and the publish
  line names what the file says differently — see the next section.

<!-- contract-truth:begin -->
## The instance is the truth; `--take` overwrites by decision

**Seeds bootstrap. The live instance runs on its data. The tenant repo
is bootstrap + export.** Decided on design `e187198f` (David,
2026-09-18): "move away from seeds mattering, except to help OSS users
bootstrap and/or to support playground; our actual BOSS instance should
be flexible to use data instead of seeds."

What it measured. `boss tenant publish` runs at EVERY services-container
start (the launcher, `infra/seed-tenant.sh`), and on 2026-09-18 four
doors overwrote a live row on every run — business calendars wholesale,
the company label, an employee's declared fields, an agent's whole row —
so an operator's edit to any of them lived exactly until the next boot.
Four other registries (classes, sensors, policy, workflows) kept the
live row and said NOTHING when the file differed, so a repo edit that
never landed was dead text. The rule this section replaces — "the
tenant's declaration wins on declared fields" (backlog `09887242`,
2026-09-17) — was measured on a repo-edited location that had not
landed, the bootstrap case; on a running instance the same overlay was
the collision. It is dropped.

How it lands.

- **Every door is insert-if-absent by default.** A row the instance
  holds is never changed by a plain publish. Each batch route takes
  `?mode=insert-if-absent|take` where its semantics live (the calendar
  and agents batches, the company mint); the employee overlay PUTs, the
  Class edit door is used, policy's `force` is set and a workflow is
  superseded only under the matching `--take`.
- **`boss tenant publish --take <registry>[,<registry>]` is the only
  overwrite.** It names `classes`, `calendars`, `company`, `policy`,
  `employees`, `agents` or `workflows`; every other door stays
  insert-if-absent on that run, and a name no door can take is refused
  with the list. Every take prints the overwritten rows field by field:
  `updated 1: <id> (location loc-hq → loc-algedonic-hq)`.
- **Every registry's line names its kept-but-differing rows**, in one
  shape: `kept: <id> differs on <fields> (the instance is the truth;
  --take <registry> overwrites)`. A registry no door overwrites
  (sensors, credentials, locations, the chart, the ledger's rules, the
  reactors) says so instead of naming a flag. This line is the decision
  surface: the operator reads what the repo says differently and
  chooses to take it, to export the instance into the repo, or to leave
  both as they are.
- **What a take does per registry.** `employees`: the row's declared
  keys are overlaid on the live row and PUT back through the people
  door (its own `people.employee.updated`), so a column the file does
  not carry rides unchanged; an explicit `null` in the file IS a
  declaration. `agents`: the declaration is the whole row (an omitted
  cap declares it unset); a declared alias another agent holds moves.
  `calendars`: header and closed-day set are replaced wholesale.
  `company`: the label. `classes`: each differing row is PUT through
  `PUT /api/classes/{kind}/{code}`. `policy`: each rule whose `scope`
  or `active` differs is re-POSTed. `workflows`: each kind whose file
  differs on a facet the drift lint compares (label, description,
  category, step count, titles, required fields, title templates) is
  published as a new version that supersedes the live one.
- **A row the tenant does not declare is never deleted**, under either
  mode, and a second publish of an unchanged directory writes nothing
  and names nothing.

The playground publishes its example tenant at every boot; with
insert-if-absent as the default that publish changes nothing on a
running instance, which is the point. Running the publish once per
database (a stamp) is the next car; `boss tenant export`, which writes
the live registries back into this shape, the one after.
<!-- contract-truth:end -->

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
migration. The starter chart is the **brewery's** — declared in
`examples/brewery/seeds/chart_of_accounts.toml` since backlog
`718ac982` (2026-09-17) and example residue everywhere else: a fresh
company instance evicts it on its first start and an existing one
through the forge verb `retire-example-reference-rows`, so a company's
books hold only what its tenant declares (the decision is in
docs/architecture-decisions.md §OSS posture). On an instance that still
carries it, a tenant's chart is published over it, insert-if-absent
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

## Credentials are declared, never carried — `seeds/credentials.toml`

A credential row is KNOWLEDGE about a secret the deployment holds —
its kind, who minted it, whose authority it carries, where the value
lives (a Secret name and key, a file path) and who reads it — so that
"what can this token do?" is a lookup and a rotation has a row to
record against (`202609031700`). The VALUE never enters: the
`[[credential]]` shape has no field one could ride in, and a key it
does not name (`value`, `token`, `secret`) is refused by the loader
and by the door, naming the key and the line and never the text
under it.

Until 2026-09-18 these rows were authored only by migrations, so every
fresh database — every OSS install — booted with one operator's forge,
Stripe and Cloudflare credential ids (audit H5, backlog `ee368d0c`).
Now they are instance data: the instance's tenant directory declares
them, `boss tenant publish` sends them through `POST
/api/credentials/batch` before the sensors (a sensor's `credential`
names one of these ids), the door lands them insert-if-absent by id
with one `credential.declared` fact per inserted row, and a row
already there keeps its `rotated_at` and notes — the rotation path's
book-keeping, which a declaration must not erase. A fresh database
holds no credential row until an instance declares one; the migration
`20260918063829-instance-data-leaves-the-platform-schema.sql` removed
the seeded rows where nothing references them. The company's own file
is the operator's commit in the tenant repository, one row per
credential the deployment's broker Secret and token files hold.

## A tenant's own reactors — `seeds/rules.toml`

A dispatcher rule is a reaction the platform runs when an event lands
(or on a schedule): the same `[[rule]]` shape the product's own rules
take under `infra/dispatcher/rules/` — `name`, a `why` naming which
standing exemption it claims (timer, threshold, external glue,
cross-protocol reactor; see that directory's README), `version`,
`on_event` or `schedule`, an optional `when` predicate, and one or
more `[[rule.do]]` handler invocations. A tenant's reactors are the
tenant's protocol data, not the product's, so they live in the
tenant's directory (backlog `458971ef`, decided by design `b64c4377`).

**One parser, two doors.** `check` reads the file with the
dispatcher's own reader (`registry::parse_raw_file` — the product's
serde shape and the product's per-rule `why` guard, plus a
uniqueness check on names that a one-file-per-rule directory pins by
construction), runs every rule through the publish door's own
`authoring::validate` (topic, predicate, arg expressions, the
trigger XOR), and checks each handler name against
`cascade::handler_emits`, the roster the product's own rules are held
to. Handler names are otherwise opaque to the dispatcher until
dispatch, so this is where a tenant learns *before* publishing that a
rule names a handler its build does not carry — INVALID, naming the
rule and the handler.

**Published append-only, through the dispatcher's own door.** For each
rule, `publish` asks the running dispatcher's
`POST /api/dispatcher/rules/_validate` first (the deployed rule
language, not the CLI's build; a refusal is its words and no draft is
left armed), reads the name's versions, and then:

- a live version **above** the file's is left alone and the line says
  `registry ahead at vN, left alone` — a version is never walked back;
- the file's version **present**, at any status, is a no-op — the line
  says `present (active)`; if the stored content differs from the
  file's, the line names the field and says to bump `version`, so a
  silent edit under an unchanged version cannot read as `present`
  forever;
- otherwise a **draft** is created carrying `source = tenant:<tenant_id>`
  and lands at the file's version (the door takes
  `max(declared, MAX + 1)`, so the SPA's editor, which sends no
  version, still gets `MAX + 1`), then **published** — the line says
  `published` or `published, superseding vN`, and the promoted row is
  checked to be the one just drafted.

**A name belongs to whoever holds it live.** `dispatcher_rules`
is one namespace with one active row per name, so a tenant drafting
`auto-park-on-gate-green` v2 would put its reaction in the product's
slot. The door refuses a draft whose `source` differs from the name's
ACTIVE or DRAFT rows, naming both owners; `publish` reads the versions
first and refuses the same way before drafting. Choose names of your
own. **A name the product retired is free for a tenant** (backlog
70bc5725, 2026-09-18): ownership is judged on live rows only, so a name
whose rows are all retired — the brewery's thirty-one reactors, which
the historical migrations insert as product rows and the boot seed
then retires — is taken over by the tenant's file. The draft lands
where the door always lands one, `max(declared, MAX + 1)`, which is
above the retired history when the file declares the version the
migration did; the line says `published as vN, taking over the name
from product`, the product's retired versions stay as history under
the name (`GET /api/dispatcher/rules/{name}/versions` shows both
sources), and the next publish reads the tenant's own row as ahead of
its file and leaves it alone. The file's version is compared against
the tenant's OWN rows only, never against the other source's history.
**A fresh instance no longer has that history to take over** (backlog
b5f21e82, 2026-09-18): the migration
`20260918022108-seed-residue-is-not-a-retirement.sql` deletes the rows
the historical migrations inserted under names no product file authors,
before the boot seed runs, so the brewery's `seeds/rules.toml` lands at
the versions it declares (`published`, v1 — v2 for the two the file
says so) and the next publish reads `present`. An instance whose tenant
took a name over BEFORE this migration converged (the playground, at
v2 above the product's retired v1) keeps both rows — the migration
leaves a name a tenant holds alone — and its publish keeps reading
`registry ahead at v2, left alone` for the file's v1. That line is
accurate and harmless: the tenant's row is live, the file's content is
what it enforces, and bumping the file's version to match would be a
change written for one instance's history.

**The product's boot seed leaves a tenant's rule alone.** The seed
that derives `dispatcher_rules` from `infra/dispatcher/rules/` retires
every enforced rule no file there names — and until this column it
read every row as the product's, so a tenant's rule died at the next
converge. A row's `source` now says whose it is: NULL is the product's
(the seed, a migration, the SPA's editor — a live rule with no file is
still retired, unchanged), `tenant:<tenant_id>` is a tenant's, and only
NULL-sourced rows are ever retired. `GET /api/dispatcher/rules` reports
`source` per rule, so a tenant's row — `authored: false` by
construction, no product file can name it — does not read as the drift
that flag exists to show.

**The worked example — what Algedonic's tenant declares.** The landing
page (`publish-the-landing-page`) has a `live` step whose evidence is
the converge that shipped the site's hash; no single workflow can
express "complete my step when *that other* packet closes", which is
the cross-protocol exemption. The tenant's `seeds/rules.toml`:

```toml
[[rule]]
name = "complete-site-live-on-converge-closed"
why = """
A cross-protocol reactor: the landing page's `live` step is proven by
the converge packet that shipped its site hash, and a step completing
from another protocol's record is exactly what a workflow cannot say.
"""
version = 1
on_event = "jobs.job.closed"
when = 'kind = "maintenance-cluster-converge"'
[[rule.do]]
handler = "jobs.complete_step_matching"
args = { kind = '"publish-the-landing-page"', step = '"live"', match_step = '"publish"', match_field = '"site_hash"', event_path = '"steps.run.site.hash"', evidence_key = '"converge"', "done_metadata.converged_by" = "id" }
```

`jobs.complete_step_matching` is being built by a sibling car: it
finds the open `publish-the-landing-page` packet whose `publish`
step's `site_hash` equals the closed converge's `steps.run.site.hash`,
completes its `live` step, and writes the converge's identity under
`evidence.converge` in the step's done metadata (the arg names above
are that handler's; read its file under `infra/dispatcher/rules/` or
its doc once it lands). Until that car lands and the CLI is rebuilt,
`check` reports this rule INVALID by handler name — which is the honest
answer — and the contract's own test fixture proves the mechanics with
`messages.notify`, a handler every build has.

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
  and since `ab192a9f` its `role` / `department` are held (above).
- Everything else — the manifest, the
  classes, the founder's employee row, the location — is OK under the
  product's own loaders.
