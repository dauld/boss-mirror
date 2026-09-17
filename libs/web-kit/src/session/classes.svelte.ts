// Class registry client — read-only loader for tenant-extensible
// taxonomies (departments, roles, account tiers, marketing-asset
// kinds, …), keyed by subject_kind.
//
// The Class registry (boss-classes-api) is the canonical source for
// per-(subject_kind, member_attribute) enumerations. Hardcoding any
// of these in SPA code defeats the whole "data over code" point of
// the registry — a brewery operator should be able to add a new
// department (or carrier, or note kind) by inserting one row, not by
// patching a Svelte file.
//
// Each subject_kind is fetched once, on demand, and cached. The boot
// path loads `employee` (departments/roles); other surfaces call
// `loadClasses('<subject_kind>')` from their mount and read via
// `classesFor('<subject_kind>', '<member_attribute>')`.

import { departmentsFromRegistry, type Department } from '../nav';

type ClassRow = Readonly<{
  subject_kind: string;
  code: string;
  display_name: string;
  parent_code: string | null;
  member_attribute: string;
  metadata: Readonly<Record<string, unknown>>;
  sort_order: number;
  retired_at: string | null;
}>;

type ClassesState =
  | { kind: 'loading' }
  | { kind: 'ready'; rows: ReadonlyArray<ClassRow> }
  | { kind: 'error' };

// Cache of loaded class sets, keyed by subject_kind. Reassigned (not
// mutated in place) on each transition so the $derived reads in callers
// re-run.
const classes = $state<{ value: Readonly<Record<string, ClassesState>> }>({
  value: {},
});

// Dedupe guard for in-flight / loaded kinds — a plain Set, deliberately
// NON-reactive. loadClasses both writes `classes.value` and needs to
// dedupe; if the dedupe read touched `classes.value`, calling loadClasses
// from a tracked $effect (as the marketing-asset surfaces do) would make
// that effect depend on the very state it writes → an infinite
// effect_update_depth_exceeded loop. Keeping the guard out of the
// reactive graph makes loadClasses safe to call from any context.
const requested = new Set<string>();

/// Load (once) the Class rows for a subject_kind. Idempotent + safe to
/// call from a tracked $effect. A failed load is NOT remembered, so a
/// later mount can retry.
export async function loadClasses(subject_kind: string): Promise<void> {
  if (requested.has(subject_kind)) return;
  requested.add(subject_kind);
  classes.value = { ...classes.value, [subject_kind]: { kind: 'loading' } };
  try {
    const r = await fetch(
      `/api/classes?subject_kind=${encodeURIComponent(subject_kind)}`,
    );
    // An answer that is not a list is an error, not a registry: since
    // the chrome bar reads departments from here on every page
    // (ce68f137), a `{data: [], total: 0}` envelope from a wrong
    // endpoint used to throw inside a derived and take the shell down.
    const body: unknown = r.ok ? await r.json() : null;
    if (Array.isArray(body)) {
      classes.value = {
        ...classes.value,
        [subject_kind]: { kind: 'ready', rows: body as ClassRow[] },
      };
    } else {
      requested.delete(subject_kind);
      classes.value = { ...classes.value, [subject_kind]: { kind: 'error' } };
    }
  } catch {
    requested.delete(subject_kind);
    classes.value = { ...classes.value, [subject_kind]: { kind: 'error' } };
  }
}

/// The tenant's departments, for the chrome bar and the sidebar's
/// group labels — `(employee, *, department)` in registry order, read
/// through the same cache as every other taxonomy (ce68f137). Empty
/// until `loadClasses('employee')` has answered; the bar then carries
/// Home (and Simulator) alone, which is the honest state of a shell
/// that does not yet know the org chart.
export function departments(): ReadonlyArray<Department> {
  return departmentsFromRegistry(classesFor('employee', 'department'));
}

/// Active (non-retired) Class rows for a (subject_kind, member_attribute),
/// sorted by sort_order. Returns an empty array while loading, on error,
/// or before the subject_kind has been loaded — callers fall back to a
/// sensible default if the list is empty.
export function classesFor(
  subject_kind: string,
  member_attribute: string,
): ReadonlyArray<ClassRow> {
  const st = classes.value[subject_kind];
  if (!st || st.kind !== 'ready') return [];
  return st.rows
    .filter(
      (r) => r.member_attribute === member_attribute && r.retired_at === null,
    )
    .slice()
    .sort((a, b) => a.sort_order - b.sort_order);
}
