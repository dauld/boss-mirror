// Departments registry client — the org chart the chrome bar renders
// its tabs from, read once at boot from `GET /api/departments`.
//
// WHY THIS IS NOT THE CLASSES CLIENT (backlog dc5788ba, design
// 32f18167). `departments()` used to sit in `classes.svelte.ts` and
// derive from `(employee, *, department)` — the EMPLOYEE DRAWER, the
// values an employee's `department` column may take. That is a
// different question from "what departments does the company have",
// and measured 2026-09-19 (backlog 80a77466) a different answer: nine
// codes against thirteen, overlapping in five. A department is a
// Subject with identity now (migration
// 20260919181324-a-department-is-a-subject.sql), so the roster has a
// table and an endpoint of its own, and this reads that.
//
// The catalog keeps the other half of the question. Which department
// owns /ux/warehouse is a ROUTING fact and stays in
// apps/web/src/shell/nav-catalog.ts; which departments exist is
// registry data and comes from here. The two are held equal by
// crates/core/boss-testing/tests/a_department_is_a_subject_sql.rs.
//
// One fetch, cached, deliberately separate from the classes cache: the
// drawer is still loaded (roles, and the policy flyout's own scope
// picker read it), and collapsing the two would put this endpoint back
// behind a `subject_kind` the departments table does not have.

import { departmentsFrom, type Department } from '../nav';

type RegistryState =
  | { kind: 'loading' }
  | { kind: 'ready'; rows: ReadonlyArray<Department> }
  | { kind: 'error' };

// Reassigned, never mutated in place, so the `$derived` reads in the
// chrome bar re-run when the roster arrives.
const registry = $state<{ value: RegistryState }>({ value: { kind: 'loading' } });

// Dedupe guard, deliberately NON-reactive — the same reason
// classes.svelte.ts keeps its own outside the reactive graph: a guard
// that read `registry.value` would make any tracked `$effect` calling
// this depend on the state it writes.
let requested = false;

/// Load (once) the departments registry. Idempotent and safe to call
/// from a tracked `$effect`. A failed load is NOT remembered, so a
/// later mount can retry.
export async function loadDepartments(): Promise<void> {
  if (requested) return;
  requested = true;
  registry.value = { kind: 'loading' };
  try {
    const r = await fetch('/api/departments');
    const body: unknown = r.ok ? await r.json() : null;
    const rows = departmentsFrom(body);
    if (rows === null) {
      requested = false;
      registry.value = { kind: 'error' };
      return;
    }
    registry.value = { kind: 'ready', rows };
  } catch {
    requested = false;
    registry.value = { kind: 'error' };
  }
}

/// The tenant's departments, in registry order — for the chrome bar's
/// tabs and the sidebar's group labels. Empty until `loadDepartments()`
/// has answered; the bar then carries Home (and Simulator) alone, which
/// is the honest state of a shell that does not yet know the org chart.
export function departments(): ReadonlyArray<Department> {
  const st = registry.value;
  return st.kind === 'ready' ? st.rows : [];
}
