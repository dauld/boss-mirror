# Design: Class registry

**Status**: living — the registry-backed taxonomy pattern; current-truth reference.

A **Class** is a logical grouping of Subjects, identified by a stable
`code` within a `subject_kind`. Roles ('ceo', 'service-tech', …) are
Classes of `employee`-kind Subjects. AccountTypes ('account',
'distributor') are Classes of `account` Subjects. Catalog asset
models are Classes of `asset` Subjects. One table seats every
taxonomy in the system.

Class is one of the supporting concepts described in
[CLAUDE.md § Primitives](../../CLAUDE.md#primitives); this doc is the
canonical implementation reference.

## Why one table

Closed Rust enums for taxonomies (`enum Role { Ceo, Cto, … }`) force
every tenant to fork core to add a value, and they tangle two
concerns: *what categories exist* (vocabulary) and *what behavior
those categories trigger* (logic). The Class registry separates them:
vocabulary lives as data, logic lives in code that *reads* the
registry.

A single `classes` table — rather than per-domain tables (`roles`,
`departments`, `account_types`, …) — lets one CRUD surface, one
authoring UI, one validation port, and one client trait serve every
taxonomy in the system. New taxonomies cost a seed migration, not
new infrastructure.

## Table

```sql
CREATE TABLE classes (
    subject_kind     TEXT NOT NULL,
    code             TEXT NOT NULL,
    display_name     TEXT NOT NULL,
    parent_code      TEXT,                    -- subclass relationship
    member_attribute TEXT,                    -- attribute-defined membership (v1)
    metadata         JSONB NOT NULL DEFAULT '{}'::jsonb,
    sort_order       INTEGER NOT NULL DEFAULT 0,
    retired_at       TIMESTAMPTZ,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at       TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (subject_kind, code),
    FOREIGN KEY (subject_kind, parent_code)
        REFERENCES classes(subject_kind, code)
        DEFERRABLE INITIALLY DEFERRED
);
```

Lives in `infra/postgres/schema/01-registries.sql`, before any domain
table that references it.

### Composite primary key `(subject_kind, code)`

Codes only need to be unique *within* a `subject_kind`. `'manager'`
can mean different Classes for `employee` and `account` if you want
it to. The composite PK enforces this without per-taxonomy tables.

### Subclass relationships stay within a `subject_kind`

`parent_code` references `(subject_kind, code)` — a Class's parent
must be of the same `subject_kind`. A `service-tech` can be a
subclass of a `tech` (both `employee`-kind), but it cannot inherit
from an `account`-kind Class.

### `member_attribute`

For attribute-defined Classes (v1), the column on the Subject whose
value matches the Class's `code`. `employees.role = 'service-tech'`
is membership in the Class `(employee, service-tech)`, declared by
`member_attribute = 'role'` on the Class row.

`NULL` is reserved for synthetic Classes with predicate or
junction-table membership (e.g. "Active techs" =
`employees.role IN (...) AND status = 'active'`). The synthetic-Class
extension lands when a real consumer needs it.

### `retired_at`, not delete

Retired Classes stay as rows so audit / history surfaces can resolve
old codes ("this employee was a `parts-buyer-2` until that role was
retired in 2026"). Filter `retired_at IS NULL` for "active" reads;
include retired rows for audit reads.

### `metadata`

JSONB for class-specific attributes. The registry doesn't constrain
shape; consumers parse what they need from `serde_json::Value`.

**Tagging conventions consumed in code:**

- `is_executive: true` — flags an `(subject_kind="employee", *)` Class
  as an executive role. Services seed an in-process cache at startup
  via `boss_classes_client::seed_executive_role_cache`. The cache
  backs `boss_core::roles::has_global_read` (gateway admin gates,
  ledger `/api/it/providers` gate, the `boss-events` audit-tail gate,
  `boss-people` scope lookups) and `boss_core::roles::is_executive`
  (the `boss-jobs` escalation router, which auto-messages executives).
- `is_management: true` — soft signal for UI ("manager view") rollups.
  Not load-bearing in core today; some tenant dashboards consume it.
- `is_system_role: true` — flags `platform-admin` and similar
  platform-shipped roles so tenant admin UIs can hide them from
  per-tenant role-editing surfaces.

Other conventions used purely for display (`color`, `icon`,
`department`, etc.) carry no platform semantics — consuming surfaces
own their interpretation.

## Rust types

`Class` + `ClassRef` live in `boss-core::primitives` alongside
`Subject` and `Part`. BOSS-core stays sqlx-free; the FromRow
intermediate lives in `boss-classes::postgres::ClassRow`.

```rust
pub struct Class {
    pub subject_kind: String,
    pub code: String,
    pub display_name: String,
    pub parent_code: Option<String>,
    pub member_attribute: Option<String>,
    pub metadata: serde_json::Value,
    pub sort_order: i32,
    pub retired_at: Option<DateTime<Utc>>,
}

pub struct ClassRef {
    pub subject_kind: String,
    pub code: String,
}
```

## Persistence

`ClassRepository` trait in `boss-classes::port`:

```rust
async fn list_for_subject_kind(&self, subject_kind: &str) -> Result<Vec<Class>>;
async fn get(&self, class_ref: &ClassRef) -> Result<Option<Class>>;
async fn exists_active(&self, class_ref: &ClassRef) -> Result<bool>;
```

`PgClasses` (Postgres adapter) and `InMemoryClasses` (test stub) both
implement it. v1 is read-only; CRUD lands when the admin UI does.

`get` returns retired rows for audit; `exists_active` excludes them
and is the hot-path validation primitive.

## HTTP API

The `boss-classes-api` service exposes a tiny read surface:

```
GET /api/classes?subject_kind=employee
GET /api/classes/{subject_kind}/{code}
GET /api/classes/{subject_kind}/{code}/exists  →  {"exists": bool}
```

## Client

`boss-classes-client::ClassesClient` is the trait other services
import to validate Class codes before accepting a write, and to load
the metadata-tagged role lists (executive, broad-account-access) at
startup. Two methods:

```rust
async fn class_exists(&self, class_ref: &ClassRef) -> Result<bool>;
async fn list_for_subject_kind(&self, subject_kind: &str) -> Result<Vec<Class>>;
```

`class_exists` is the write-time gate; `list_for_subject_kind` feeds
the startup cache seeders (`seed_executive_role_cache`,
`seed_broad_account_access_role_cache`).

`ReqwestClassesClient` for production; `FakeClassesClient` for tests
(`permissive()` accepts everything, `with(allowed)` gates to a
specific allow-list, `with_classes(...)` drives `list_for_subject_kind`).

## Validation pattern (the consumer side)

A service whose schema carries a closed-enum CHECK constraint
(e.g. `employees.role IN ('ceo', 'cto', …)`) lifts the constraint
into application-layer registry validation in four steps:

1. Add `boss-classes-client` as a dep.
2. Constructor takes `Arc<dyn ClassesClient>`.
3. On every write that sets the relevant attribute, call
   `class_exists(&ClassRef::new(subject_kind, code))` before commit.
   Return the existing domain error variant if the Class is not
   found (`Conflict` for boss-people, equivalent for others).
4. Tests pass `Arc::new(FakeClassesClient::permissive())`.

After the validation is wired and proven, **drop the CHECK
constraint from the schema and make the registry-API URL a
required config field**. At that point the registry is the only
defense and the closed Rust enum can be lifted to a String-newtype
in a follow-up commit (since the schema no longer enforces a
closed set, the type system shouldn't either).

**Which attributes validate against the registry:**

- `employees.role`, `employees.department`,
  `employees.employment_type`, and `employees.status` validate
  against the matching `(subject_kind='employee', member_attribute=…)`
  Class rows; their CHECK constraints are dropped and
  `classes_api_url` is required in `PeopleApiConfig`.
- Account-team roles (`boss-accounts`, `account_team_members.rs`)
  validate via `class_exists("employee", role)`.
- Catalog asset models (`boss-catalog/src/http.rs`) validate
  `ClassRef::new("asset", category)`.

Two deliberate non-targets, recorded so passes don't re-open them:

- `employees.skill_level` is **not a Class** — it's a numeric range
  (1..=5), not a closed enum, so its CHECK stays.
- `employees.location` resolves to the **Location primitive**, not a
  Class: `WorkLocation::{Hq, Remote, Field}` becomes an FK into a
  `locations` table with hierarchy + geo + timezone (see TODO.md),
  not a `classes` row.

`accounts.account_type` has its CHECK dropped, `account_type` Class
rows seeded, and the `accounts.rs` write path validates it against the
registry (`check_account_type` → `class_exists(ClassRef::new("account",
…))`). Remaining tenant taxonomies stay on closed CHECK constraints
until each is lifted by the four-step pattern above.

## Authoring

v1 doesn't ship CRUD endpoints; the registry seeds via migrations.
The admin authoring UI (`/admin/classes` or
`/admin/classes/<subject_kind>`) lands when a tenant needs to create
or retire codes outside of a deploy. At that point the
`ClassRepository` trait grows `create`, `update`, `retire` methods
and the HTTP router grows POST/PUT.

## When to add a new Class kind

A new `subject_kind` row family is appropriate when:

- The kind is a Subject (per [CLAUDE.md § Primitives](../../CLAUDE.md#primitives)
  — an identity-bearing thing BOSS tracks).
- You have a tenant-extensible taxonomy you want to author, retire,
  display, and validate uniformly with all the others.

Concretely, **before** a new taxonomy gets its own table, ask: "is
this just a Class registry seed for a new `subject_kind`?" The
answer is almost always yes.

## When NOT to use the Class registry

Three flavors of "category-shaped" data the registry does *not*
fit:

1. **Subjects themselves.** A specific employee, a specific account,
   a specific device unit — those are Subjects, not Classes. Subjects
   have lifecycle, events, Jobs. Classes don't.
2. **Free-form tags.** If the value space is unbounded user input
   (e.g. arbitrary skill strings), a tags table is more appropriate
   than a registry. Registries are for vocabulary the tenant
   curates.
3. **Per-row configuration.** Settings whose value is specific to one
   Subject instance and shares no vocabulary with any other — the
   `reorder_point` / `reorder_qty` numbers on an inventory item, a
   per-account credit limit — are Subject attributes, not Class
   membership. Use the Class registry only when the *category* itself
   is the thing that has display + grouping metadata.

   The test is the second sentence, not "does it vary per row". A
   short closed list that the tenant curates and the UI labels,
   sorts, or groups by **is** Class material even though every
   Subject carries exactly one of them. `account.tier`
   (Platinum/Gold/Silver) and `account.account_type` are both
   Classes for that reason, seeded in `01-registries.sql` with
   display names and sort orders and validated on the accounts write
   path. An earlier revision of this section cited `tier` as the
   counter-example; that was wrong by this section's own test, and
   the implementation never followed it.

## Retirement flow

Retiring a code:

1. UPDATE the row to set `retired_at = now()`.
2. Existing rows that reference the code keep resolving via `get`
   (audit history works).
3. New writes referencing the code get rejected at
   `exists_active` time.
4. Authoring UI hides retired codes from create-form dropdowns;
   includes them in audit timelines.

Retired Classes can be unretired (set `retired_at = NULL`); the row
is the source of truth either way.
