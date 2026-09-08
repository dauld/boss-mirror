# Used-Device-Shop Business Domain (BOSS example tenant)

This is the worked example BOSS was built against — a generic
business that sells, services, and resells used physical devices
that need sophisticated diagnostics and repair. The tenant builds
on top of the platform-supplied **Equipment KB** (`boss-catalog`
+ `boss-assets`) and **People** (`boss-people`); the
tenant-specific crates (`boss-commerce`, `boss-inventory`,
`boss-shipping`) layer the goods-and-services flows that aren't
universal. It is not the BOSS
product — BOSS core is generic; this is one tenant's
instantiation.

The shop sells, refurbishes, services, and supports complex
physical devices, primarily to professional service-account
customers. Used-system refurbishment is the cornerstone margin
line; new sales, service contracts, parts, and repair round it
out.

## Install

One command on a fresh Ubuntu 24.04 VM:

```sh
sudo TENANT=device-shop /opt/boss/infra/bootstrap-vm.sh
```

That stands up the same service stack the brewery uses, then runs
`boss-used-device-shop-engine prepare` — the tenant's converged
seed step (sister of `boss-brewery-sim prepare`), which publishes
the whole model from this directory through the public API, in
dependency order: Class rows (`seeds/classes.toml`), the company
identity Subject, policy grants (`seeds/policy_rules.toml`), the
76-person roster (`data/employees.json`), the device catalog
(`data/catalog.json`), and finally every Workflow in
`seeds/workflows.toml` via the shared `workflow-design`
publish path. Idempotent — re-run it freely.

To re-seed an already-running stack by hand:

```sh
BOSS_SIM_SEEDS_DIR=/opt/boss/examples/used-device-shop/seeds \
    boss-used-device-shop-engine prepare
```

Unlike the brewery there is no live sim daemon: once prepared, work
on this tenant is opened and driven by human and agent actors
through the normal surfaces. The `boss-used-device-shop-engine`
*library* drives the same seed bundle as a deterministic day-loop
in tests and offline runs (`tests/shape_driven_smoke.rs`).

## How to read this doc

BOSS frames the business as a **human-powered state machine**:
the people below are the CPUs that execute work, the nouns are
the cells of memory they read and write, and the activities are
the programs they run. The rest of this file is the "business
inputs to the machine" — keep the framing in mind while reading
it and it'll stay consistent with the code. Full vocabulary in
[docs/design/human-powered-state-machine.md](../../docs/design/human-powered-state-machine.md).

---

## People

| Role | Views they need |
|---|---|
| **CEO / Exec** | Business pulse, exceptions worth attention, strategic signals |
| **Sales rep** | Pipeline, customer history, product catalog with commerce info |
| **Customer success** | Account health, service SLA, churn risk, installed devices per account |
| **Refurbishment tech** | Incoming-unit queue, refurb checklist, parts needed, time on bench |
| **Service tech** | Open tickets by urgency, system history, service manuals, parts, calibration records |
| **QA tech** | Units awaiting final QA, calibration logs, certificate generation |
| **Warehouse worker** | Receive / pick / pack / ship queues, bin locations, cycle counts |
| **Parts buyer** | Stock levels, open POs, lead times, vendor performance |
| **Finance** | AR aging, margin by product/channel, cost of service |
| **Compliance** | Regulator registrations, reportable events, system history files |
| **Account contact** | (external) their systems, their service contracts, request service |

## Things (nouns)

In BOSS-primitive terms, every noun below is a Subject (identity-bearing)
or a Class (taxonomy of Subjects). See
[CLAUDE.md](../../CLAUDE.md) and
[docs/architecture/01-primitives.mmd](../../docs/architecture/01-primitives.mmd)
for the platform-level definitions.

- **system_model** — a Class of `system`-kind Subjects in the
  Equipment KB (`boss-catalog`). Powers e-commerce, sales
  configurator, service manual library.
- **System** — a Subject of kind `system` (SYS-{id}). One per
  tenant-identified unit. Has a lineage: received → refurbished →
  QA'd → sold → installed → serviced → returned → ...
- **Account** — a Subject of kind `account` (a customer organization
  with one or more systems).
- **Contact** — a person at an account or vendor (specialist,
  purchasing agent, account manager). Carried as an
  `AttributePart` on its parent Account or Vendor.
- **Employee** — a Subject of kind `employee` with a role Class
  and skills.
- **PartSku** — a spare-part SKU catalog entry (Class of `part`).
  Note: distinct from the platform `Part` primitive (the
  composition role); we keep the SKU naming for clarity.
- **InventoryItem** — a physical quantity of a PartSku at a
  WarehouseLocation. An `AttributePart` on the part SKU's KB.
- **Consumable** — single-use item tied to a system_model (tips,
  modules). Stored alongside parts in the Equipment KB.
- **Document** — manuals, spec sheets, regulator certs,
  calibration records, photos. Lives in `boss-content`.
- **Vendor** — a Subject of kind `vendor`. External suppliers
  of parts or OEM systems.
- **Job** — a bounded unit of coordinated work (refurb, field
  service, sale, procurement, campaign, onboarding). Decomposes
  into Steps owned by different people/teams.
- **Step** — a typed unit of work on a Job, owned by one person,
  with optional sign-off gate.

## Activities (processes)

| Process | Flow |
|---|---|
| **Acquire used system** | intake → triage → refurb queue → refurb work → QA → ready-to-sell |
| **Refurb workflow** | disassembly → component check → parts replacement → reassembly → calibration → certification |
| **Sell system** | lead → qualified → quoted → configured → order → shipped → installed |
| **Service call** | ticket opened → diagnosis → parts ordered (if needed) → repair → QA → returned-to-service |
| **Preventive maintenance** | scheduled → dispatched → performed on-site or depot → report filed → next preventive maintenance scheduled |
| **Decommission** | end-of-life → harvest usable parts → dispose |
| **Warranty claim** | customer reports → validate → service path |
| **Parts procurement** | need identified → PO → receive → put-away → consume |

## Places

- **Receiving bay** — incoming used units, incoming parts
- **Refurb bench** — active refurbishment work
- **QA bay** — calibration, certification
- **Shipping bay** — outbound
- **Parts racks** — inventory by bin
- **Showroom** — demo units for sales
- **Field** — customer sites (for on-site service)

## Concepts (contracts, documents of record)

- **Quote / SalesOrder / Invoice**
- **WarrantyContract / ServiceContract** — what's covered, for how long
- **SystemHistoryRecord** — regulator-traceable; every service event on a system is logged
- **CalibrationCertificate** — issued at QA, carries date + tech + reading set
- **Signal / Insight** — editorial surfacing to humans (from Exec view)

---

## Data layer

- Postgres holds all structured business data: systems, accounts, orders, tickets, invoices, inventory, shipments, jobs, steps, service agreements, employees, audit log. 102 tables total; see `infra/postgres/schema/`.
- NATS carries events across services. `boss-nats` is the event bus adapter; every domain service publishes on write via a `DomainPublisher`, and `PgAuditWriter` persists every published event to the `audit_log` table alongside the NATS publish.
- Domain types and ports (traits) live in Rust crates under `crates/`. Platform crates (catalog, assets, people, messages, jobs) are reused as-is; the tenant-specific crates layer the goods-and-services flows.
  - **Platform** — `boss-catalog` (Equipment KB: asset_models, parts, consumables), `boss-assets` (per-unit System Subjects + event history), `boss-people` (employees, contacts, accounts), `boss-messages` (inbox), `boss-jobs` (Jobs + Steps).
  - **Tenant** — `boss-commerce` (opportunities, invoices, revenue lines, service agreements), `boss-inventory` (parts stock, POs, vendors), `boss-shipping` (inbound/outbound tracking).
- Each crate defines domain types and ports; `InMemory*` adapters implement them for fast tests, and `Pg*` adapters implement them against Postgres for production.
- The knowledge base is the hub of the model: (1) it's the most shared entity — sales, service, refurb, QA, customers all read it; (2) it's slow-changing reference data; (3) it serves both commerce (customer-facing) and service (internal) use cases in one record. See `README.md` for the current service map.
