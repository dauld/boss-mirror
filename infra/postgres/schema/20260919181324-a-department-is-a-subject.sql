-- 20260919181324-a-department-is-a-subject.sql — a department is a
-- Subject kind with identity, and its function is Class data on that
-- kind.
--
-- Origin: backlog 5987302f (design 32f18167, answered by David
-- 2026-09-19, anchor `what-is-a-department`). MEASURED that day:
-- `GET /api/classes?subject_kind=department` returned zero rows and no
-- `department` subject kind existed, while the fourteen department
-- codes lived as string literals in one frontend file
-- (apps/web/src/shell/nav-catalog.ts) and
-- infra/platform/workflows/page-audit.toml required a `department`
-- field it describes as "the department Class code" and types as a free
-- string validated against nothing. Forty-seven page-audit packets were
-- opened the same day carrying thirteen of those codes.
--
-- WHY A SUBJECT KIND AND NOT ONLY A CLASS. The test that settled it, in
-- the design's own words: "the warehouse ran its retro late" is
-- unsayable if a department is only a Class, because a Class cannot
-- carry state. A department has identity, owns work, has a head, a
-- charter, protocols and a retro cadence, and things happen TO it — so
-- it needs a row a Job can point at. Its FUNCTION (operations, revenue,
-- support, governance) is the taxonomy over those rows, and a taxonomy
-- is Class data (CLAUDE.md §9).
--
-- THE AXIS. `person` — the "who" axis — beside `company`, which is
-- already there as "the tenant organization itself". A department is
-- the part of that organization that acts; it is not a place, an
-- object, or an intangible like a contract.
--
-- NOT birth-by-job, for the same reason as `node`: a department exists
-- whether or not anyone opens a packet about it, so a Job naming a
-- department id that was never declared stays fail-closed at the
-- uniform jobs existence gate.
--
-- WHY A MIGRATION AND NOT A PLATFORM BUNDLE. infra/lint/migrations-
-- declare-schema-only.sh names the four registries whose rows moved out
-- of migrations (stations, step_plugins, cadence_rules,
-- delivery_policy); `classes` and `subject_kinds` are not among them,
-- and 144-estate-subjects.sql is the precedent this file follows —
-- kind, domain table, rows and identity rows in one place. When the
-- department roster becomes tenant-published (the seeds carry
-- examples/*/seeds/classes.*), these rows become residue the same way
-- the example reference rows did.
--
-- SCOPE. This declares the kind, the taxonomy and the rows. It does NOT
-- rewire nav-catalog.ts to read them (backlog dc5788ba), does not split
-- the employee Class drawer (a45ab09d), and does not touch
-- page-audit.toml's schema. Those are separate cars.

INSERT INTO subject_kinds (kind, label, description, owning_team, sort_order, parent_kind) VALUES
    ('department', 'Department', 'A standing part of the organization: it owns work, has a head, a charter, protocols and a retro cadence. Its function (operations / revenue / support / governance) is Class data on this kind. Specializes `person` beside `company` — a department is the part of the organization that acts.', 'platform', 16, 'person')
ON CONFLICT (kind) DO NOTHING;

-- The function taxonomy. Four codes, and the point of the axis is to
-- answer "what is this department FOR" without reading its charter:
-- who brings money in, who does the work the company exists to do, who
-- serves the people involved, and who steers and holds the rest to
-- account.
INSERT INTO classes (subject_kind, code, display_name, member_attribute, sort_order) VALUES
    ('department', 'operations',  'Operations',  'function', 10),
    ('department', 'revenue',     'Revenue',     'function', 20),
    ('department', 'support',     'Support',     'function', 30),
    ('department', 'governance',  'Governance',  'function', 40)
ON CONFLICT (subject_kind, code) DO NOTHING;

-- A department's `id` IS its code — the string a page-audit packet
-- carries in `metadata.department` and the catalog spells as a route's
-- `app`. Keeping them one value is what makes the correspondence
-- checkable at all (crates/core/boss-testing/tests/
-- a_department_is_a_subject_sql.rs holds it equal to the catalog).
--
-- `head_employee_id` is a SOFT reference, not an FK: employees land in
-- a later schema file and a registry that cannot be seeded before the
-- people module is a registry with a load order. `charter` and the head
-- are nullable because no department has written one yet; filling them
-- is the department's own work, not a migration's.
CREATE TABLE IF NOT EXISTS departments (
    id               TEXT PRIMARY KEY,
    label            TEXT NOT NULL,
    -- Validated against (subject_kind='department',
    -- member_attribute='function') in the Class registry, the same
    -- contract employees.role and locations.kind carry.
    function         TEXT NOT NULL,
    charter          TEXT,
    head_employee_id TEXT,
    metadata         JSONB NOT NULL DEFAULT '{}'::jsonb,
    sort_order       INTEGER NOT NULL DEFAULT 0,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    retired_at       TIMESTAMPTZ
);

CREATE INDEX IF NOT EXISTS departments_function ON departments(function) WHERE retired_at IS NULL;

-- THE ROSTER, and it is not a wish list: these are the codes the
-- catalog assigns its routes today and the codes the forty-seven
-- page-audit packets of 2026-09-19 already carry. `home` is the
-- fourteenth catalog `app` and is deliberately NOT here — Home surfaces
-- are cross-cutting personal work rather than a department's, IT builds
-- them, and apps/web/scripts/open-page-audits.ts already maps `home`
-- (and `simulator`) to `it` in one place, which is why no packet
-- carries `home`. A `home` row would be a department nothing would ever
-- name.
INSERT INTO departments (id, label, function, sort_order) VALUES
    ('it',           'IT',           'operations', 1),
    ('executive',    'Executive',    'governance', 10),
    ('finance',      'Finance',      'governance', 20),
    ('qa',           'QA',           'governance', 30),
    ('sales',        'Sales',        'revenue',    40),
    ('marketing',    'Marketing',    'revenue',    50),
    ('support',      'Support',      'support',    60),
    ('service',      'Service',      'support',    70),
    ('people',       'People',       'support',    80),
    ('production',   'Production',   'operations', 90),
    ('warehouse',    'Warehouse',    'operations', 100),
    ('distribution', 'Distribution', 'operations', 110),
    ('maintenance',  'Maintenance',  'operations', 120)
ON CONFLICT (id) DO NOTHING;

-- Identity rows, so a Job may be about a department — which is the
-- whole claim: "the warehouse ran its retro late" needs `warehouse` to
-- be a Subject.
INSERT INTO subjects (kind, id, label)
    SELECT 'department', id, label FROM departments
ON CONFLICT (kind, id) DO NOTHING;
