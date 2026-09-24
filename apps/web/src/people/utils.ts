// Pure helpers on employee data. Ported from apps/web/src/people/utils.ts.

import { humanizeClassCode, type Certification, type Employee, type EmployeeId } from './types';
import { appNow } from '@boss/web-kit/sim-clock';

/// One Status filter button: a status code (`null` = the rows with no
/// status yet, which render as "unknown"), its label, and how many
/// roster rows it holds.
export type StatusBucket = Readonly<{ code: string | null; label: string; count: number }>;

/// The roster's Status buttons, built from the (employee, status)
/// Classes rather than a hand-written pair (backlog 01c268d6: the old
/// Active / On leave / All left `terminated` and null-status rows
/// reachable only under All, and uncounted). Every status Class gets a
/// button in the registry's order; a status the roster holds that no
/// Class names (the registry is still loading, or failed) is appended
/// under its humanized code; an Unknown bucket follows when any row has
/// no status. So every row sits in exactly one bucket.
export function statusBuckets(
  roster: ReadonlyArray<Employee>,
  statusClasses: ReadonlyArray<Readonly<{ code: string; display_name: string }>>,
): StatusBucket[] {
  const countOf = (code: string | null): number =>
    roster.filter((e) => e.status === code).length;
  const classCodes = new Set(statusClasses.map((c) => c.code));
  const unclassed = Array.from(
    new Set(
      roster
        .map((e) => e.status)
        .filter((s): s is NonNullable<typeof s> => s !== null && !classCodes.has(s)),
    ),
  );
  const nulls = countOf(null);
  return [
    ...statusClasses.map((c) => ({ code: c.code, label: c.display_name, count: countOf(c.code) })),
    ...unclassed.map((s) => ({ code: s, label: humanizeClassCode(s), count: countOf(s) })),
    ...(nulls > 0 ? [{ code: null, label: 'Unknown', count: nulls }] : []),
  ];
}

/// A roster filter on one attribute: All, or one code — `null` being
/// the rows that have no value for it. A union rather than a sentinel
/// string, so no class code can collide with the All button.
export type CodeFilter = Readonly<{ kind: 'all' } | { kind: 'code'; code: string | null }>;

/// One Department filter button: a department code (`null` = the rows
/// with no department), its label, and how many rows it holds.
export type DepartmentBucket = Readonly<{ code: string | null; label: string; count: number }>;

/// The roster's Department buttons, built from `rows` — the rows the
/// Status selection admits — rather than from the active rows (backlog
/// 1410f145; page audit 0c0265a3 GAP 6): built from active rows, the
/// counts contradicted the table under On leave or All, a department
/// whose people were all on leave or terminated had no button, and a
/// row with no department could not be selected by department. Codes
/// sort alphabetically; a No department bucket follows when any row
/// has none, so every row sits in exactly one bucket. The `selected`
/// department keeps its button at zero when the rows hold none of it,
/// so a Status change never hides the filter that empties the table.
export function departmentBuckets(
  rows: ReadonlyArray<Employee>,
  selected: CodeFilter,
): DepartmentBucket[] {
  const countOf = (code: string | null): number =>
    rows.filter((e) => e.department === code).length;
  const pinned = selected.kind === 'code' ? [selected.code] : [];
  const codes = new Set([...rows.map((e) => e.department), ...pinned]);
  const named = Array.from(codes)
    .filter((d): d is string => d !== null)
    .sort();
  return [
    ...named.map((d) => ({ code: d, label: humanizeClassCode(d), count: countOf(d) })),
    ...(codes.has(null) ? [{ code: null, label: 'No department', count: countOf(null) }] : []),
  ];
}

export function tenureYears(employee: Employee, today: Date = appNow()): number {
  // Identity-first: no hire_date yet (an un-onboarded record) means no
  // measurable tenure.
  if (!employee.hire_date) return 0;
  return (
    (today.getTime() - new Date(employee.hire_date).getTime()) /
    (1000 * 60 * 60 * 24 * 365)
  );
}

export function directReports(
  managerId: EmployeeId,
  employees: ReadonlyArray<Employee>,
): Employee[] {
  return employees.filter((e) => e.manager_id === managerId);
}

export function expiringCerts(
  daysAhead: number,
  employees: ReadonlyArray<Employee>,
  today: Date = appNow(),
): Array<{ employee: Employee; cert: Certification }> {
  const cutoff = new Date(today);
  cutoff.setDate(cutoff.getDate() + daysAhead);
  const out: Array<{ employee: Employee; cert: Certification }> = [];
  for (const e of employees) {
    for (const c of e.certifications) {
      if (!c.expires_on) continue;
      const exp = new Date(c.expires_on);
      if (exp >= today && exp <= cutoff) out.push({ employee: e, cert: c });
    }
  }
  return out;
}
